use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender, broadcast};
use async_channel::{Sender as MpscSender, unbounded};
use parking_lot::{Mutex as PlMutex, RwLock};
use russh::{
    Channel, ChannelMsg,
    client::{self, Msg},
};
use tokio::task::AbortHandle;
use tokio::{runtime::Runtime, select, sync::Mutex as TokioMutex};

use crabport_core::credential::ProxyConfig;
use crabport_sftp::CrabPortSftp;
use crabport_terminal::terminal::{BackendEvent, RemoteMetrics, RemoteStatus};

use crate::handler::SshHandler;
use crate::keys::decode_private_key;
use crate::known_hosts::KnownHosts;
use crate::monitor::monitor_loop;
use crate::session::SshConnectionInfo;
use crate::transfer::SftpTransferHandle;

/// Connect a russh session over a (possibly proxied) stream.
///
/// Thin bridge: builds the stream via `crabport_proxy::connect` (direct,
/// SOCKS5, or HTTP/HTTPS CONNECT), then hands it to
/// `russh::client::connect_stream`.
async fn connect_russh<H>(
    config: Arc<client::Config>,
    proxy: &Option<ProxyConfig>,
    target_host: &str,
    target_port: u16,
    handler: H,
) -> Result<client::Handle<H>, H::Error>
where
    H: russh::client::Handler + Send + 'static,
{
    let stream = crabport_proxy::connect(proxy, target_host, target_port)
        .await
        .map_err(|e| <H::Error as From<russh::Error>>::from(russh::Error::from(e)))?;
    client::connect_stream(config, stream, handler).await
}

// Re-export the public handler-API types so existing callers using
// `crabport_ssh::backend::HostKeyInfo` / `HostKeyVerifier` keep working after
// the split.
#[allow(unused_imports)]
pub use crate::handler::{HostKeyInfo, HostKeyVerifier, HostKeyVerifyFuture};

// ---------------------------------------------------------------------------
// Tokio runtime for russh (russh internally requires tokio)
// ---------------------------------------------------------------------------

/// Tokio runtime shared by all SSH backends in this process. russh requires
/// a tokio runtime, so we lazily create one and reuse it across connects.
pub static TOKIO: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("failed to create tokio runtime for SSH"));

// ---------------------------------------------------------------------------
// Internal command queue
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

// ---------------------------------------------------------------------------
// Shared monitor state
// ---------------------------------------------------------------------------

/// State shared between the SSH event loop and the monitor task.
pub(crate) struct MonitorState {
    pub(crate) status: RemoteStatus,
    pub(crate) metrics: RemoteMetrics,
}

// ---------------------------------------------------------------------------
// SshBackend
// ---------------------------------------------------------------------------

/// SSH terminal backend.
///
/// Connects via TCP, authenticates, opens a PTY session, then enters a
/// single `tokio::select!` event loop that handles reads, writes, and
/// resizes — no mutex needed because only one task touches the channel.
pub struct SshBackend {
    pub(crate) command_tx: MpscSender<Command>,
    pub(crate) event_tx: BroadcastSender<BackendEvent>,
    pub(crate) _event_rx: InactiveReceiver<BackendEvent>,
    pub(crate) monitor: Arc<RwLock<MonitorState>>,
    pub(crate) _on_status: Arc<dyn Fn(String) + Send + Sync>,
    pub(crate) handle: Arc<TokioMutex<Option<Arc<TokioMutex<client::Handle<SshHandler>>>>>>,
    pub(crate) sftp_entries: Arc<RwLock<Option<Arc<Vec<(String, bool)>>>>>,
    pub(crate) sftp_cwd: Arc<RwLock<Option<Arc<String>>>>,
    /// Cached SFTP subsystem session. Reused across navigations so we don't
    /// pay the cost of opening a fresh SFTP channel (and leaking the old
    /// one) on every `sftp_navigate` call. Lazily (re)connected if `None`,
    /// e.g. after the server closes the channel or on first use.
    pub(crate) sftp_session: Arc<TokioMutex<Option<crabport_sftp::SftpBackend>>>,
    /// Abort handles for in-flight SFTP transfer tasks (download / upload /
    /// delete) spawned via `TOKIO.spawn` in the `CrabPortTerminal` impl.
    /// Tracked so that `Command::Close` can cancel them instead of letting
    /// them outlive the terminal: a transfer task holds its own
    /// `SftpTransferHandle` (with `Arc` clones of the SSH handle + SFTP
    /// session cache + event sink), so without explicit cancellation the
    /// underlying SSH connection would stay alive — and keep streaming
    /// bytes / opening new SFTP channels — until the transfer finished on
    /// its own, even after the user closed the terminal tab.
    pub(crate) transfer_tasks: Arc<PlMutex<Vec<AbortHandle>>>,
}

impl SshBackend {
    pub fn new(
        info: SshConnectionInfo,
        cols: u16,
        rows: u16,
        on_status: Arc<dyn Fn(String) + Send + Sync>,
        verifier: Option<HostKeyVerifier>,
    ) -> Self {
        let (event_tx, event_rx) = broadcast(1024);
        let _event_rx = event_rx.deactivate();
        let (command_tx, command_rx) = unbounded::<Command>();

        let monitor = Arc::new(RwLock::new(MonitorState {
            status: RemoteStatus::Connecting,
            metrics: RemoteMetrics::default(),
        }));
        let handle: Arc<TokioMutex<Option<Arc<TokioMutex<client::Handle<SshHandler>>>>>> =
            Arc::new(TokioMutex::new(None));

        let sftp_entries: Arc<RwLock<Option<Arc<Vec<(String, bool)>>>>> =
            Arc::new(RwLock::new(None));
        let sftp_entries2 = sftp_entries.clone();
        let sftp_cwd: Arc<RwLock<Option<Arc<String>>>> = Arc::new(RwLock::new(None));
        let sftp_cwd2 = sftp_cwd.clone();
        let sftp_session: Arc<TokioMutex<Option<crabport_sftp::SftpBackend>>> =
            Arc::new(TokioMutex::new(None));
        let sftp_session2 = sftp_session.clone();
        let transfer_tasks: Arc<PlMutex<Vec<AbortHandle>>> = Arc::new(PlMutex::new(Vec::new()));
        let transfer_tasks_close = transfer_tasks.clone();

        let event_tx2 = event_tx.clone();
        let monitor2 = monitor.clone();
        let on_status2 = on_status.clone();
        let handle_for_spawn = handle.clone();
        let host_for_handler = info.host.clone();
        let port_for_handler = info.port;
        let verifier_for_handler = verifier.clone();

        TOKIO.spawn(async move {
            // ---- Connect ----
            let addr = format!("{}:{}", info.host, info.port);
            #[cfg(debug_assertions)]
            tracing::info!("SSH: connecting to {}", addr);
            on_status2(format!("Connecting to {}", addr));

            // Open the known_hosts store. Failure here is non-fatal — we
            // just fall back to prompting on every connect.
            let known_hosts = match KnownHosts::open() {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("SSH: could not open known_hosts store ({e})");
                    None
                }
            };

            let handler = SshHandler {
                host: host_for_handler,
                port: port_for_handler,
                known_hosts,
                verifier: verifier_for_handler,
            };

            let config = Arc::new(client::Config::default());
            let mut sh = match connect_russh(config, &info.proxy, &info.host, info.port, handler).await {
                Ok(sh) => {
                    on_status2("TCP connection established".into());
                    sh
                }
                Err(e) => {
                    tracing::error!("SSH: connect failed: {e}");
                    {
                        let mut m = monitor2.write();
                        m.status = RemoteStatus::Disconnected;
                    }
                    let _ = event_tx2
                        .broadcast(BackendEvent::Error(e.to_string()))
                        .await;
                    return;
                }
            };

            #[cfg(debug_assertions)]
            tracing::info!(
                "SSH: auth decision — uses_key_auth={}, private_key={}, has_passphrase={}, username={}",
                info.uses_key_auth(),
                info.private_key.is_some(),
                info.passphrase.is_some(),
                info.username,
            );
            if info.uses_key_auth() {
                on_status2("Authenticating with public key...".into());

                let key_str = info.private_key.as_deref().unwrap_or("");
                #[cfg(debug_assertions)]
                tracing::info!("SSH: private key length={}, starts_with_BEGIN={}", key_str.len(), key_str.contains("BEGIN"));
                let key_pair = match decode_private_key(key_str, info.passphrase.as_deref()) {
                    Ok(kp) => kp,
                    Err(e) => {
                        tracing::error!("SSH: failed to decode private key: {e}");
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2
                            .broadcast(BackendEvent::Error(format!(
                                "Public key decode failed: {e}"
                            )))
                            .await;
                        return;
                    }
                };

                let auth_result = sh
                    .authenticate_publickey(&info.username, Arc::new(key_pair))
                    .await;

                #[cfg(debug_assertions)]
                tracing::info!("SSH: publickey auth result = {:?}", auth_result);
                match auth_result {
                    Ok(true) => {
                        on_status2("Public key authentication succeeded".into());
                    }
                    Ok(false) => {
                        tracing::error!("SSH: key auth rejected");
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2
                            .broadcast(BackendEvent::Error(
                                "Public key authentication failed".into(),
                            ))
                            .await;
                        return;
                    }
                    Err(e) => {
                        tracing::error!("SSH: key auth failed: {e}");
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2
                            .broadcast(BackendEvent::Error(format!(
                                "Public key authentication failed: {e}"
                            )))
                            .await;
                        return;
                    }
                }
            } else {
                #[cfg(debug_assertions)]
                tracing::info!("SSH: using password auth (private_key is None)");
                on_status2("Authenticating with password...".into());
                match sh
                    .authenticate_password(&info.username, &info.password)
                    .await
                {
                    Ok(true) => {
                        #[cfg(debug_assertions)]
                        tracing::info!("SSH: password auth succeeded");
                        on_status2("Password authentication succeeded".into());
                    }
                    Ok(false) => {
                        tracing::error!("SSH: password auth rejected");
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2
                            .broadcast(BackendEvent::Error("Password authentication failed".into()))
                            .await;
                        return;
                    }
                    Err(e) => {
                        tracing::error!("SSH: auth failed: {e}");
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2
                            .broadcast(BackendEvent::Error(format!(
                                "Password authentication failed: {e}"
                            )))
                            .await;
                        return;
                    }
                }
            }

            // ---- Open session channel ----
            on_status2("Opening session channel...".into());
            let mut channel: Channel<Msg> = match sh.channel_open_session().await {
                Ok(ch) => {
                    on_status2("Session channel opened".into());
                    ch
                }
                Err(e) => {
                    tracing::error!("SSH: open session failed: {e}");
                    {
                        let mut m = monitor2.write();
                        m.status = RemoteStatus::Disconnected;
                    }
                    let _ = event_tx2
                        .broadcast(BackendEvent::Error(format!(
                            "Session channel open failed: {e}"
                        )))
                        .await;
                    return;
                }
            };

            // ---- Request PTY ----
            on_status2(format!("Requesting PTY ({}x{})...", cols, rows));
            let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
            if let Err(e) = channel
                .request_pty(false, &term, cols as u32, rows as u32, 0, 0, &[])
                .await
            {
                tracing::error!("SSH: PTY request failed: {e}");
                {
                    let mut m = monitor2.write();
                    m.status = RemoteStatus::Disconnected;
                }
                let _ = event_tx2
                    .broadcast(BackendEvent::Error(format!("PTY request failed: {e}")))
                    .await;
                return;
            }
            on_status2("PTY allocated".into());

            // ---- Start shell ----
            on_status2("Starting shell...".into());
            if let Err(e) = channel.request_shell(true).await {
                tracing::error!("SSH: shell request failed: {e}");
                {
                    let mut m = monitor2.write();
                    m.status = RemoteStatus::Disconnected;
                }
                let _ = event_tx2
                    .broadcast(BackendEvent::Error(format!("Shell request failed: {e}")))
                    .await;
                return;
            }
            on_status2("Shell started".into());

            // Mark as connected
            {
                let mut m = monitor2.write();
                m.status = RemoteStatus::Connected;
            }

            // ---- Try SFTP ----
            // Open the SFTP subsystem once and cache it for the lifetime of
            // the connection. Subsequent `sftp_navigate` calls reuse this
            // session instead of opening (and leaking) a new channel each
            // time.
            match crabport_sftp::SftpBackend::connect(&sh).await {
                Ok(sftp) => {
                    tracing::info!("SSH: SFTP subsystem available");
                    // Resolve cwd
                    match sftp.canonicalize(".").await {
                        Ok(cwd) => {
                            *sftp_cwd2.write() = Some(Arc::new(cwd));
                        }
                        Err(e) => tracing::warn!("SSH: SFTP canonicalize failed ({e})"),
                    }
                    match sftp.read_dir(".").await {
                        Ok(entries) => {
                            *sftp_entries2.write() = Some(Arc::new(entries));
                        }
                        Err(e) => tracing::warn!("SSH: SFTP read_dir failed ({e})"),
                    }
                    *sftp_session2.lock().await = Some(sftp);
                }
                Err(e) => tracing::warn!("SSH: SFTP subsystem not available ({e})"),
            }

            // ---- Spawn monitor task ----
            // Wrap the Handle in Arc<TokioMutex> so the monitor task can use it.
            // Handle is not Clone, so we share it via Arc.
            let handle_for_monitor = Arc::new(TokioMutex::new(sh));

            // Share the same Arc with SshBackend for SFTP access.
            *handle_for_spawn.lock().await = Some(handle_for_monitor.clone());

            let monitor_for_task = monitor2.clone();
            TOKIO.spawn(async move {
                monitor_loop(handle_for_monitor, info, monitor_for_task).await;
            });

            // ---- Event loop (read + cmd via tokio::select!) ----
            loop {
                select! {
                    msg = channel.wait() => {
                        match msg {
                            Some(ChannelMsg::Data { data }) => {
                                let _ = event_tx2
                                    .broadcast(BackendEvent::Data(data.to_vec()))
                                    .await;
                            }
                            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                                #[cfg(debug_assertions)]
                                tracing::info!("SSH: channel closed by remote");
                                {
                                    let mut m = monitor2.write();
                                    m.status = RemoteStatus::Disconnected;
                                }
                                let _ = event_tx2.broadcast(BackendEvent::Closed).await;
                                return;
                            }
                            _ => {}
                        }
                    }
                    cmd = command_rx.recv() => {
                        match cmd {
                            Ok(Command::Write(data)) => {
                                if let Err(_e) = channel.data(Cursor::new(data)).await {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("SSH: write error: {_e}");
                                }
                            }
                            Ok(Command::Resize(cols, rows)) => {
                                if let Err(_e) = channel
                                    .window_change(cols as u32, rows as u32, 0, 0)
                                    .await
                                {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("SSH: window change error: {_e}");
                                }
                            }
                            Ok(Command::Close) | Err(_) => {
                                // Abort any in-flight SFTP transfer tasks before
                                // tearing down the channel. Each transfer task
                                // holds its own `SftpTransferHandle` (with `Arc`
                                // clones of the SSH handle + SFTP session cache),
                                // so without aborting them the SSH connection
                                // would stay alive — and the transfers would
                                // keep streaming / opening new SFTP channels —
                                // until they finished on their own, even though
                                // the user closed the terminal. Aborting drops
                                // those `Arc` clones at the next `.await` point,
                                // letting the SSH handle actually be released.
                                let aborted = {
                                    let mut tasks = transfer_tasks_close.lock();
                                    let v = tasks.drain(..).collect::<Vec<_>>();
                                    v
                                };
                                #[cfg(debug_assertions)]
                                tracing::info!(
                                    "SSH: close — aborting {} in-flight transfer task(s)",
                                    aborted.len()
                                );
                                for handle in aborted {
                                    handle.abort();
                                }
                                let _ = channel.eof().await;
                                // Close the cached SFTP session (if any) so the
                                // server-side channel is torn down cleanly
                                // rather than lingering until the SSH
                                // connection itself drops. Note: a transfer
                                // task that already `take`-or-opened the cached
                                // session may still hold a live session; that's
                                // fine — it'll be dropped (and its channel
                                // closed) when the aborted task's future is
                                // dropped along with its `SftpTransferHandle`.
                                if let Some(sftp) = sftp_session2.lock().await.take() {
                                    let _ = sftp.close().await;
                                }
                                {
                                    let mut m = monitor2.write();
                                    m.status = RemoteStatus::Disconnected;
                                }
                                let _ = event_tx2.broadcast(BackendEvent::Closed).await;
                                return;
                            }
                        }
                    }
                }
            }
        });

        Self {
            command_tx,
            event_tx,
            _event_rx,
            monitor,
            _on_status: on_status,
            handle,
            sftp_entries,
            sftp_cwd,
            sftp_session,
            transfer_tasks,
        }
    }

    // -----------------------------------------------------------------------
    // File transfer (implicit gzip + remote tmp staging)
    // -----------------------------------------------------------------------
    //
    // Both directions follow the same pattern to keep the wire format
    // compressed without changing what's stored at the final destinations:
    //
    //   upload   local_orig --gzip--> remote_tmp.gz --gunzip--> remote_orig
    //   download remote_orig --gzip--> remote_tmp.gz --gunzip--> local_orig
    //
    // The client-side gzip happens in-memory over the SFTP file stream (see
    // `SftpBackend::upload_file_gz` / `download_file_gz`), so no local tmp
    // file is needed. The remote side needs a tmp file because we drive the
    // (de)compression via `ssh exec`, which can't stream into a pre-opened
    // SFTP handle — it has to read from a path.
    //
    // Remote tmp paths use a per-call random token to avoid collisions
    // between concurrent transfers. They live under `/tmp` (overridable via
    // `$TMPDIR` on the remote, which `mktemp` honours).

    /// Download a remote file into `local_path`.
    ///
    /// See [`crate::transfer::sftp_download_impl`] for the gzip/tmp staging
    /// flow. This is a thin wrapper that supplies the shared fields as a
    /// [`SftpTransferHandle`].
    pub async fn sftp_download(&self, remote_path: &str, local_path: &str) -> anyhow::Result<()> {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        crate::transfer::sftp_download_impl(&backend, remote_path, local_path).await
    }

    /// Upload a local file into `remote_path`.
    ///
    /// See [`crate::transfer::sftp_upload_impl`] for the gzip/tmp staging
    /// flow. This is a thin wrapper that supplies the shared fields as a
    /// [`SftpTransferHandle`].
    pub async fn sftp_upload(&self, local_path: &str, remote_path: &str) -> anyhow::Result<()> {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        crate::transfer::sftp_upload_impl(&backend, local_path, remote_path).await
    }

    // -----------------------------------------------------------------------
    // In-flight transfer task tracking
    // -----------------------------------------------------------------------

    /// Spawn a transfer task (download / upload / delete) on the shared
    /// `TOKIO` runtime and track its [`AbortHandle`] so that `Command::Close`
    /// can cancel it.
    ///
    /// Each transfer task holds its own [`SftpTransferHandle`] (with `Arc`
    /// clones of the SSH handle + SFTP session cache + event sink), so without
    /// explicit cancellation the SSH connection would stay alive — and the
    /// transfers would keep streaming / opening new SFTP channels — until they
    /// finished on their own, even after the user closed the terminal.
    ///
    /// Tracked handles are aborted (a no-op if the task already completed) and
    /// drained in the `Command::Close` arm of the SSH event loop. We don't
    /// auto-unregister on natural completion because `AbortHandle` exposes no
    /// `is_finished` check; this is fine in practice since the vec is bounded
    /// by the number of transfers a session kicks off (typically small) and is
    /// fully cleared on close.
    pub(crate) fn spawn_transfer<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let join = TOKIO.spawn(future);
        let abort = join.abort_handle();
        self.transfer_tasks.lock().push(abort);
    }
}
