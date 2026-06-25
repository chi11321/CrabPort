use std::{io::Cursor, sync::Arc, sync::LazyLock};

use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};
use async_channel::{Sender as MpscSender, unbounded};
use crabport_terminal::terminal::{BackendEvent, CrabPortTerminal};
use russh::{
    Channel, ChannelMsg,
    client::{self, Msg},
};
use tokio::{runtime::Runtime, select};
use tracing::{debug, error, info, warn};

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
}

impl SshBackend {
    pub fn new(info: SshConnectionInfo, cols: u16, rows: u16) -> Self {
        let (event_tx, event_rx) = broadcast(1024);
        let _event_rx = event_rx.deactivate();
        let (command_tx, command_rx) = unbounded::<Command>();

        let event_tx2 = event_tx.clone();

        TOKIO.spawn(async move {
            // ---- 1. Connect ----
            let config = Arc::new(client::Config::default());
            let addr = format!("{}:{}", info.host, info.port);
            let mut sh = match client::connect(config, &addr, SshHandler).await {
                Ok(sh) => {
                    debug!("SSH: TCP connected to {addr}");
                    sh
                }
                Err(e) => {
                    error!("SSH: connect failed: {e}");
                    let _ = event_tx2
                        .broadcast(BackendEvent::Error(e.to_string()))
                        .await;
                    return;
                }
            };

            // ---- 2. Authenticate ----
            if let Err(e) = sh
                .authenticate_password(&info.username, &info.password)
                .await
            {
                error!("SSH: auth failed: {e}");
                let _ = event_tx2
                    .broadcast(BackendEvent::Error(format!("auth failed: {e}")))
                    .await;
                return;
            }
            debug!("SSH: authenticated as {}", info.username);

            // ---- 3. Open session channel ----
            let mut channel: Channel<Msg> = match sh.channel_open_session().await {
                Ok(ch) => {
                    debug!("SSH: session channel opened");
                    ch
                }
                Err(e) => {
                    error!("SSH: open session failed: {e}");
                    let _ = event_tx2
                        .broadcast(BackendEvent::Error(e.to_string()))
                        .await;
                    return;
                }
            };

            // ---- 4. Request PTY ----
            let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
            if let Err(e) = channel
                .request_pty(false, &term, cols as u32, rows as u32, 0, 0, &[])
                .await
            {
                error!("SSH: PTY request failed: {e}");
                let _ = event_tx2
                    .broadcast(BackendEvent::Error(e.to_string()))
                    .await;
                return;
            }
            debug!("SSH: PTY allocated {cols}x{rows}");

            // ---- 5. Start shell ----
            if let Err(e) = channel.request_shell(true).await {
                error!("SSH: shell request failed: {e}");
                let _ = event_tx2
                    .broadcast(BackendEvent::Error(e.to_string()))
                    .await;
                return;
            }
            debug!("SSH: shell started");

            // ---- 6. Event loop (read + cmd via tokio::select!) ----
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
                                info!("SSH: channel closed by remote");
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
                                    warn!("SSH: write error: {e}");
                                }
                            }
                            Ok(Command::Resize(cols, rows)) => {
                                if let Err(e) = channel
                                    .window_change(cols as u32, rows as u32, 0, 0)
                                    .await
                                {
                                    warn!("SSH: window change error: {e}");
                                }
                            }
                            Ok(Command::Close) | Err(_) => {
                                let _ = channel.eof().await;
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
        }
    }
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
}

impl Drop for SshBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}
