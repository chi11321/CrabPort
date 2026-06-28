use std::{
    io::Cursor,
    sync::{Arc, LazyLock},
};

use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};
use async_channel::{Sender as MpscSender, unbounded};
use crabport_terminal::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, MemoryStats, NetworkStats, RemoteMetrics,
    RemoteStatus,
};
use parking_lot::RwLock;
use russh::{
    Channel, ChannelMsg,
    client::{self, Msg},
    keys::key::KeyPair,
};
use tokio::{runtime::Runtime, select, sync::Mutex as TokioMutex};

use crate::session::SshConnectionInfo;

// ---------------------------------------------------------------------------
// Tokio runtime for russh (russh internally requires tokio)
// ---------------------------------------------------------------------------

static TOKIO: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("failed to create tokio runtime for SSH"));

// ---------------------------------------------------------------------------
// Internal command queue
// ---------------------------------------------------------------------------

enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

// ---------------------------------------------------------------------------
// Shared monitor state
// ---------------------------------------------------------------------------

/// State shared between the SSH event loop and the monitor task.
struct MonitorState {
    status: RemoteStatus,
    metrics: RemoteMetrics,
}

// ---------------------------------------------------------------------------
// SSH client handler
// ---------------------------------------------------------------------------

struct SshHandler;

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // TODO: proper host-key verification (TOFU / known_hosts)
        Ok(true)
    }
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
    command_tx: MpscSender<Command>,
    event_tx: BroadcastSender<BackendEvent>,
    _event_rx: InactiveReceiver<BackendEvent>,
    monitor: Arc<RwLock<MonitorState>>,
    _on_status: Arc<dyn Fn(String) + Send + Sync>,
}

impl SshBackend {
    pub fn new(
        info: SshConnectionInfo,
        cols: u16,
        rows: u16,
        on_status: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        let (event_tx, event_rx) = broadcast(1024);
        let _event_rx = event_rx.deactivate();
        let (command_tx, command_rx) = unbounded::<Command>();

        let monitor = Arc::new(RwLock::new(MonitorState {
            status: RemoteStatus::Connecting,
            metrics: RemoteMetrics::default(),
        }));

        let event_tx2 = event_tx.clone();
        let monitor2 = monitor.clone();
        let info_for_monitor = info.clone();
        let on_status2 = on_status.clone();

        TOKIO.spawn(async move {
            // ---- 1. Connect ----
            let addr = format!("{}:{}", info.host, info.port);
            #[cfg(debug_assertions)]
            tracing::info!("SSH: connecting to {}", addr);
            on_status2(format!("Connecting to {}", addr));

            let config = Arc::new(client::Config::default());
            let mut sh = match client::connect(config, &addr, SshHandler).await {
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

            // ---- 3. Open session channel ----
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

            // ---- 4. Request PTY ----
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

            // ---- 5. Start shell ----
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

            // ---- 6. Spawn monitor task ----
            // Wrap the Handle in Arc<TokioMutex> so the monitor task can use it.
            // Handle is not Clone, so we share it via Arc.
            let handle_for_monitor = Arc::new(TokioMutex::new(sh));
            let monitor_for_task = monitor2.clone();
            let info_for_task = info_for_monitor;
            TOKIO.spawn(async move {
                monitor_loop(handle_for_monitor, info_for_task, monitor_for_task).await;
            });

            // ---- 7. Event loop (read + cmd via tokio::select!) ----
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
                                if let Err(e) = channel.data(Cursor::new(data)).await {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("SSH: write error: {e}");
                                }
                            }
                            Ok(Command::Resize(cols, rows)) => {
                                if let Err(e) = channel
                                    .window_change(cols as u32, rows as u32, 0, 0)
                                    .await
                                {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("SSH: window change error: {e}");
                                }
                            }
                            Ok(Command::Close) | Err(_) => {
                                let _ = channel.eof().await;
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
        }
    }
}

// ---------------------------------------------------------------------------
// Monitor loop — periodically collects latency / memory / network via SSH exec
// ---------------------------------------------------------------------------

async fn monitor_loop(
    handle: Arc<TokioMutex<client::Handle<SshHandler>>>,
    _info: SshConnectionInfo,
    monitor: Arc<RwLock<MonitorState>>,
) {
    let mut prev_net_sent: u64 = 0;
    let mut prev_net_recv: u64 = 0;

    // ---- First collection immediately on connection ----
    {
        let h = handle.lock().await;
        let latency_ms = measure_latency(&h).await;
        let memory = collect_memory(&h).await;
        let network = collect_network(&h).await;

        if let Some(net) = network {
            prev_net_sent = net.bytes_sent;
            prev_net_recv = net.bytes_recv;
        }

        let mut m = monitor.write();
        m.metrics = RemoteMetrics {
            latency_ms,
            memory,
            network: None, // No rate on first tick
        };
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Skip if disconnected
        {
            let m = monitor.read();
            if m.status == RemoteStatus::Disconnected {
                return;
            }
        }

        let h = handle.lock().await;

        // ---- Latency: measure RTT of a small exec command ----
        let latency_ms = measure_latency(&h).await;

        // ---- Memory: parse /proc/meminfo ----
        let memory = collect_memory(&h).await;

        // ---- Network: parse /proc/net/dev ----
        let raw_network = collect_network(&h).await;
        let network = raw_network.map(|net| {
            let rate_sent = net.bytes_sent.saturating_sub(prev_net_sent);
            let rate_recv = net.bytes_recv.saturating_sub(prev_net_recv);
            prev_net_sent = net.bytes_sent;
            prev_net_recv = net.bytes_recv;
            NetworkStats {
                bytes_sent: rate_sent,
                bytes_recv: rate_recv,
            }
        });

        // ---- Update shared state ----
        {
            let mut m = monitor.write();
            m.metrics = RemoteMetrics {
                latency_ms,
                memory,
                network,
            };
        }
    }
}

/// Measure round-trip latency by executing `echo ping` over SSH.
async fn measure_latency(handle: &client::Handle<SshHandler>) -> Option<u32> {
    let start = std::time::Instant::now();
    match handle.channel_open_session().await {
        Ok(mut ch) => {
            if ch.exec(true, "echo ping").await.is_err() {
                return None;
            }
            // Drain output until channel closes
            loop {
                match ch.wait().await {
                    Some(ChannelMsg::Data { .. }) => {}
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
            let elapsed = start.elapsed().as_millis() as u32;
            Some(elapsed)
        }
        Err(_) => None,
    }
}

/// Collect remote memory stats via `cat /proc/meminfo`.
async fn collect_memory(handle: &client::Handle<SshHandler>) -> Option<MemoryStats> {
    let output = exec_and_read(handle, "cat /proc/meminfo").await?;
    let mut total: u64 = 0;
    let mut available: u64 = 0;

    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let value = parts[1].parse::<u64>().unwrap_or(0);
        // /proc/meminfo values are in kB
        if parts[0].starts_with("MemTotal") {
            total = value * 1024;
        } else if parts[0].starts_with("MemAvailable") {
            available = value * 1024;
        }
    }

    if total == 0 {
        return None;
    }

    Some(MemoryStats {
        total,
        used: total.saturating_sub(available),
    })
}

/// Collect remote network stats via `cat /proc/net/dev`.
/// Sums across all interfaces.
async fn collect_network(handle: &client::Handle<SshHandler>) -> Option<NetworkStats> {
    let output = exec_and_read(handle, "cat /proc/net/dev").await?;
    let mut bytes_recv: u64 = 0;
    let mut bytes_sent: u64 = 0;

    for line in output.lines() {
        let trimmed = line.trim();
        // Skip header lines
        if !trimmed.contains(':') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        let fields: Vec<&str> = parts[1].split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        // Fields: receive bytes (0) | ... | transmit bytes (8) | ...
        bytes_recv += fields[0].parse::<u64>().unwrap_or(0);
        bytes_sent += fields[8].parse::<u64>().unwrap_or(0);
    }

    Some(NetworkStats {
        bytes_sent,
        bytes_recv,
    })
}

/// Execute a command over SSH and read all its stdout output.
async fn exec_and_read(handle: &client::Handle<SshHandler>, cmd: &str) -> Option<String> {
    let mut ch = handle.channel_open_session().await.ok()?;
    if ch.exec(true, cmd).await.is_err() {
        return None;
    }

    let mut output = Vec::new();
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => {
                output.extend_from_slice(&data);
            }
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }

    String::from_utf8(output).ok()
}

// ---------------------------------------------------------------------------
// Private key decoding
// ---------------------------------------------------------------------------

fn decode_private_key(
    key_str: &str,
    passphrase: Option<&str>,
) -> Result<KeyPair, Box<dyn std::error::Error + Send + Sync>> {
    // Try PEM-encoded key first (OpenSSH format: "-----BEGIN OPENSSH PRIVATE KEY-----")
    if key_str.contains("BEGIN") {
        let pair = russh::keys::decode_secret_key(key_str, passphrase)?;
        return Ok(pair);
    }

    // Otherwise treat as a raw key file path or content — try as file path first
    if let Ok(content) = std::fs::read_to_string(key_str) {
        let pair = russh::keys::decode_secret_key(&content, passphrase)?;
        return Ok(pair);
    }

    Err("cannot decode private key: not a valid PEM key or file path".into())
}

// ---------------------------------------------------------------------------
// CrabPortTerminal impl
// ---------------------------------------------------------------------------

impl CrabPortTerminal for SshBackend {
    fn write(&self, data: &[u8]) {
        let _ = self.command_tx.try_send(Command::Write(data.to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.command_tx.try_send(Command::Resize(cols, rows));
    }

    fn close(&self) {
        let _ = self.command_tx.try_send(Command::Close);
    }

    fn subscribe(&self) -> BroadcastReceiver<BackendEvent> {
        self.event_tx.new_receiver()
    }

    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// CrabPortMonitor impl
// ---------------------------------------------------------------------------

impl CrabPortMonitor for SshBackend {
    fn status(&self) -> RemoteStatus {
        self.monitor.read().status
    }

    fn metrics(&self) -> RemoteMetrics {
        self.monitor.read().metrics
    }
}

impl Drop for SshBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}
