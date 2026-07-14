//! `TelnetBackend` — raw-TCP + IAC telnet transport.
//!
//! Connects via [`crabport_proxy::connect`] (direct / SOCKS5 / HTTP CONNECT /
//! HTTPS CONNECT) so proxy support is shared with SSH for free. The resulting
//! stream is a tokio `AsyncRead + AsyncWrite`, so — like `crabport_ssh` — we
//! drive all I/O on a shared tokio runtime (`TOKIO`) and bridge to the
//! frontend's `broadcast`/`async-channel` primitives.
//!
//! Wire format (RFC 854): a single async task runs a combined read+write
//! select! loop. It:
//!
//! - Sends proactive negotiation (DO ECHO, DO SUPPRESS_GO_AHEAD, WILL NAWS,
//!   WILL TERMINAL_TYPE) after connect so the server knows we're a real
//!   terminal.
//! - Runs an IAC state machine that strips command bytes from the visible
//!   output, responds to DO/WILL with sensible accept/refuse decisions, and
//!   replies to TERMINAL-TYPE subnegotiations with the local `$TERM` value.
//! - Auto-detects `login:` / `Password:` prompts in the visible data stream
//!   and replies with the stored credentials automatically.
//! - Sends NAWS updates on terminal resize.
//!
//! Compared to the original v1 which refused all options and required manual
//! login typing, this backend behaves like a proper telnet client: typed
//! input is echoed, the server knows our terminal size and type, and
//! credentials are sent automatically (mirroring how `SshBackend` handles
//! password auth).

use std::sync::{Arc, LazyLock};

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender, broadcast};
use async_channel::{Sender as MpscSender, unbounded};
use parking_lot::RwLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{runtime::Runtime, select};

use crabport_terminal::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, RemoteMetrics, RemoteStatus,
};

use crate::session::TelnetConnectionInfo;

// ---------------------------------------------------------------------------
// Telnet protocol constants (RFC 854, RFC 857, RFC 1073, RFC 1091)
// ---------------------------------------------------------------------------

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

/// Telnet options we negotiate.
mod opt {
    pub const ECHO: u8 = 1;
    pub const SUPPRESS_GO_AHEAD: u8 = 3;
    pub const TERMINAL_TYPE: u8 = 24;
    pub const NAWS: u8 = 31;
}

// ---------------------------------------------------------------------------
// Tokio runtime (the proxy stream is tokio-based, same as SSH)
// ---------------------------------------------------------------------------

/// Tokio runtime shared by all telnet backends in this process. The proxy
/// crate returns a tokio `AsyncRead + AsyncWrite` stream, so we need a tokio
/// runtime to drive it — same rationale as `crabport_ssh::backend::TOKIO`.
pub static TOKIO: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("failed to create tokio runtime for telnet"));

// ---------------------------------------------------------------------------
// Internal command queue (frontend → backend)
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Command {
    /// User-typed data to send to the server.
    Write(Vec<u8>),
    /// Terminal resize notification.
    Resize(u16, u16),
    /// Close the connection gracefully.
    Close,
}

// ---------------------------------------------------------------------------
// Shared monitor state
// ---------------------------------------------------------------------------

/// State shared between the I/O tasks and the `CrabPortMonitor` impl.
struct MonitorState {
    status: RemoteStatus,
    metrics: RemoteMetrics,
}

// ---------------------------------------------------------------------------
// IAC parser state machine
// ---------------------------------------------------------------------------

/// Parser state carried across read boundaries. Telnet IAC sequences may
/// straddle `read` calls, so we can't process a buffer in isolation.
#[derive(Clone, Copy, PartialEq)]
enum IacState {
    /// Normal visible data.
    Normal,
    /// Just consumed an IAC (255); the next byte is the command.
    Iac,
    /// Consumed `IAC WILL`; next byte is the option being offered.
    Will,
    /// Consumed `IAC WONT`; next byte is the option (no reply needed).
    Wont,
    /// Consumed `IAC DO`; next byte is the option being requested.
    Do,
    /// Consumed `IAC DONT`; next byte is the option (no reply needed).
    Dont,
    /// Inside a subnegotiation (`IAC SB …`); skip until `IAC SE`.
    Sb,
    /// Inside a subnegotiation and just consumed an IAC; next byte is either
    /// SE (end) or 255 (escaped literal) or another command.
    SbIac,
}

// ---------------------------------------------------------------------------
// Auto-login state machine
// ---------------------------------------------------------------------------

/// Tracks which credential the server is currently prompting for.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LoginPhase {
    /// Not yet seen a prompt.
    Idle,
    /// Saw "login:" — waiting to send username.
    LoginPrompt,
    /// Saw "Password:" — waiting to send password.
    PasswordPrompt,
    /// Credentials sent; no more auto-login.
    Done,
}

/// Auto-login buffer that accumulates plain-text output and scans for
/// `login:` / `Password:` prompts.
struct AutoLogin {
    phase: LoginPhase,
    /// Accumulated plain-text bytes since the last flush. We scan the
    /// trailing bytes for prompt patterns.
    buf: Vec<u8>,
    /// Whether the startup command has been sent yet. It is dispatched once,
    /// after auto-login completes (the no-credentials case is handled by
    /// the caller before the event loop starts).
    startup_sent: bool,
}

impl AutoLogin {
    fn new() -> Self {
        Self {
            phase: LoginPhase::Idle,
            buf: Vec::new(),
            startup_sent: false,
        }
    }

    /// Feed a chunk of plain-text data that *will* be displayed to the user.
    /// Returns `Some(reply)` when a prompt is detected and a credential
    /// should be sent (or, once login completes, the startup command); `None`
    /// otherwise.
    fn feed(&mut self, data: &[u8], info: &TelnetConnectionInfo) -> Option<Vec<u8>> {
        // Once login is complete, dispatch the startup command exactly once.
        if self.phase == LoginPhase::Done && !self.startup_sent {
            self.startup_sent = true;
            if !info.startup_command.is_empty() {
                return Some(crabport_core::credential::build_startup_command_bytes(
                    &info.startup_command,
                ));
            }
        }

        self.buf.extend_from_slice(data);
        // Keep only the last 256 bytes — prompt detection only needs recent
        // output, and unbounded growth on long sessions would be wasteful.
        if self.buf.len() > 256 {
            let excess = self.buf.len() - 256;
            self.buf.drain(..excess);
        }
        match self.phase {
            LoginPhase::Done => None,
            LoginPhase::Idle => {
                if has_login_prompt(&self.buf) {
                    self.phase = LoginPhase::LoginPrompt;
                    self.buf.clear();
                    Some(format!("{}\r\n", info.username).into_bytes())
                } else {
                    None
                }
            }
            LoginPhase::LoginPrompt => {
                if has_password_prompt(&self.buf) {
                    self.phase = LoginPhase::PasswordPrompt;
                    self.buf.clear();
                    Some(format!("{}\r\n", info.password).into_bytes())
                } else {
                    None
                }
            }
            LoginPhase::PasswordPrompt => {
                // After sending password, mark done — no more auto-login.
                self.phase = LoginPhase::Done;
                None
            }
        }
    }
}

/// Scan the trailing text for a `login:` prompt (case-insensitive).
fn has_login_prompt(buf: &[u8]) -> bool {
    let lower: Vec<u8> = buf.iter().map(|b| b.to_ascii_lowercase()).collect();
    let window = if lower.len() > 64 {
        &lower[lower.len() - 64..]
    } else {
        &lower
    };
    // "login:" anywhere in the last chunk.
    window.windows(6).any(|w| w == b"login:")
}

/// Scan the trailing text for a `Password:` prompt (case-insensitive).
fn has_password_prompt(buf: &[u8]) -> bool {
    let lower: Vec<u8> = buf.iter().map(|b| b.to_ascii_lowercase()).collect();
    let window = if lower.len() > 64 {
        &lower[lower.len() - 64..]
    } else {
        &lower
    };
    window.windows(9).any(|w| w == b"password:")
}

// ---------------------------------------------------------------------------
// TelnetBackend
// ---------------------------------------------------------------------------

/// Telnet terminal backend.
///
/// Connects over raw TCP (optionally through a proxy), negotiates ECHO +
/// NAWS + TERMINAL-TYPE with the server, auto-responds to login prompts
/// with stored credentials, and bridges the resulting byte stream to the
/// frontend via the standard `CrabPortTerminal` trait.
pub struct TelnetBackend {
    command_tx: MpscSender<Command>,
    event_tx: BroadcastSender<BackendEvent>,
    _event_rx: InactiveReceiver<BackendEvent>,
    monitor: Arc<RwLock<MonitorState>>,
    #[allow(dead_code)]
    _on_status: Arc<dyn Fn(String) + Send + Sync>,
}

impl TelnetBackend {
    /// Create and connect a telnet backend.
    ///
    /// `on_status` receives human-readable connection-state updates
    /// ("Connecting to …", "TCP connection established", …) mirroring
    /// `SshBackend::new`.
    pub fn new(
        info: TelnetConnectionInfo,
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
        let on_status2 = on_status.clone();

        TOKIO.spawn(async move {
            let addr = format!("{}:{}", info.host, info.port);
            #[cfg(debug_assertions)]
            tracing::info!("telnet: connecting to {}", addr);
            on_status2(format!("Connecting to {}", addr));

            let mut stream = match crabport_proxy::connect(&info.proxy, &info.host, info.port).await
            {
                Ok(s) => {
                    #[cfg(debug_assertions)]
                    tracing::info!("telnet: TCP connected to {}", addr);
                    on_status2("TCP connection established".into());
                    {
                        let mut m = monitor2.write();
                        m.status = RemoteStatus::Connected;
                    }
                    s
                }
                Err(e) => {
                    tracing::error!("telnet: connect failed: {e}");
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

            // ---- Initial negotiation ----
            // After TCP connect, proactively request options that improve the
            // terminal experience:
            //   IAC WILL ECHO          → "I'm willing to echo"
            //   IAC WILL SUPPRESS_GA   → "skip Go-Ahead protocol"
            //   IAC WILL NAWS          → "I'll send window size"
            //   IAC DO TERMINAL_TYPE   → "tell me your terminal type"
            let init = [
                IAC,
                WILL,
                opt::ECHO,
                IAC,
                WILL,
                opt::SUPPRESS_GO_AHEAD,
                IAC,
                WILL,
                opt::NAWS,
                IAC,
                DO,
                opt::TERMINAL_TYPE,
            ];
            if let Err(e) = stream.write_all(&init).await {
                tracing::warn!("telnet: initial negotiation write error: {e}");
            }
            let _ = stream.flush().await;

            // Send initial NAWS payload so the server knows our size from
            // the start (before any DO NAWS negotiation completes).
            let naws = naws_payload(cols, rows);
            if let Err(e) = stream.write_all(&naws).await {
                tracing::warn!("telnet: initial NAWS write error: {e}");
            }
            let _ = stream.flush().await;

            // ---- Startup command (no-credentials case) ----
            // When there are no credentials to auto-send, dispatch the
            // startup command immediately after negotiation. The
            // auto-login path also handles the post-credential case inside
            // the event loop, so `startup_sent` prevents double-send.
            let mut auto_login = AutoLogin::new();
            if info.username.is_empty()
                && info.password.is_empty()
                && !info.startup_command.is_empty()
            {
                let payload =
                    crabport_core::credential::build_startup_command_bytes(&info.startup_command);
                if !payload.is_empty() {
                    #[cfg(debug_assertions)]
                    tracing::info!(
                        "telnet: sending startup command ({} bytes, no-credentials path)",
                        payload.len()
                    );
                    if let Err(e) = stream.write_all(&payload).await {
                        tracing::warn!("telnet: startup command write error: {e}");
                    }
                    let _ = stream.flush().await;
                }
                auto_login.startup_sent = true;
            }

            // ---- Combined read+write event loop ----
            // Single tokio::select! handles reading from the socket,
            // user commands (write/resize/close), and auto-login credential
            // sends. No split — reads and writes share the same stream.
            let mut buf = [0u8; 8192];
            let mut iac_state = IacState::Normal;
            let mut data_out: Vec<u8> = Vec::with_capacity(8192);
            let mut neg_out: Vec<u8> = Vec::with_capacity(256);

            loop {
                select! {
                    // ---- Socket read ----
                    result = stream.read(&mut buf) => {
                        match result {
                            Ok(0) => {
                                #[cfg(debug_assertions)]
                                tracing::info!("telnet: EOF from server");
                                flush_data(&event_tx2, &mut data_out).await;
                                {
                                    let mut m = monitor2.write();
                                    m.status = RemoteStatus::Disconnected;
                                }
                                let _ = event_tx2.broadcast(BackendEvent::Closed).await;
                                return;
                            }
                            Ok(n) => {
                                // Parse IAC from the socket; visible bytes go
                                // into data_out, negotiation replies go into
                                // neg_out.
                                neg_out.clear();
                                process_iac(&buf[..n], &mut iac_state, &mut data_out, &mut neg_out);

                                // Write negotiation replies to the socket
                                // first — they are raw IAC bytes, NOT visible
                                // terminal output.
                                if !neg_out.is_empty() {
                                    #[cfg(debug_assertions)]
                                    tracing::debug!(
                                        "telnet: sending {} negotiation bytes",
                                        neg_out.len()
                                    );
                                    if let Err(e) = stream.write_all(&neg_out).await {
                                        tracing::warn!("telnet: negotiation write error: {e}");
                                    }
                                    let _ = stream.flush().await;
                                }

                                // Broadcast visible data (IAC already
                                // stripped) to the frontend.
                                if !data_out.is_empty() {
                                    let chunk = std::mem::take(&mut data_out);
                                    // Check for login prompts in the visible
                                    // output. If detected, send credentials
                                    // inline.
                                    if let Some(reply) = auto_login.feed(&chunk, &info) {
                                        #[cfg(debug_assertions)]
                                        tracing::debug!(
                                            "telnet: auto-login sending (phase={:?})",
                                            auto_login.phase
                                        );
                                        if let Err(e) = stream.write_all(&reply).await {
                                            tracing::warn!("telnet: auto-login write error: {e}");
                                        }
                                        let _ = stream.flush().await;
                                    }
                                    #[cfg(debug_assertions)]
                                    tracing::debug!("telnet read: {} data bytes", chunk.len());
                                    let _ = event_tx2.broadcast(BackendEvent::Data(chunk)).await;
                                }
                            }
                            Err(e) => {
                                tracing::error!("telnet read error: {e}");
                                flush_data(&event_tx2, &mut data_out).await;
                                {
                                    let mut m = monitor2.write();
                                    m.status = RemoteStatus::Disconnected;
                                }
                                let _ = event_tx2
                                    .broadcast(BackendEvent::Error(e.to_string()))
                                    .await;
                                return;
                            }
                        }
                    }

                    // ---- User commands ----
                    cmd = command_rx.recv() => {
                        match cmd {
                            Ok(Command::Write(data)) => {
                                if let Err(e) = stream.write_all(&data).await {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("telnet: write error: {e}");
                                }
                                let _ = stream.flush().await;
                            }
                            Ok(Command::Resize(cols, rows)) => {
                                let naws = naws_payload(cols, rows);
                                if let Err(e) = stream.write_all(&naws).await {
                                    #[cfg(debug_assertions)]
                                    tracing::warn!("telnet: NAWS write error: {e}");
                                }
                                let _ = stream.flush().await;
                            }
                            Ok(Command::Close) | Err(_) => {
                                #[cfg(debug_assertions)]
                                tracing::info!("telnet: closing connection");
                                let _ = stream.shutdown().await;
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

        let _ = (cols, rows);

        Self {
            command_tx,
            event_tx,
            _event_rx,
            monitor,
            _on_status: on_status,
        }
    }
}

/// Flush any accumulated visible data before emitting a terminal event.
async fn flush_data(event_tx: &BroadcastSender<BackendEvent>, data_out: &mut Vec<u8>) {
    if !data_out.is_empty() {
        let chunk = std::mem::take(data_out);
        let _ = event_tx.broadcast(BackendEvent::Data(chunk)).await;
    }
}

/// Build a NAWS subnegotiation payload: `IAC SB NAWS <cols-hi> <cols-lo>
/// <rows-hi> <rows-lo> IAC SE`.
fn naws_payload(cols: u16, rows: u16) -> [u8; 9] {
    [
        IAC,
        SB,
        opt::NAWS,
        (cols >> 8) as u8,
        cols as u8,
        (rows >> 8) as u8,
        rows as u8,
        IAC,
        SE,
    ]
}

/// Run the IAC state machine over one read chunk, appending visible bytes to
/// `data_out` and queuing negotiation replies via `resp` (which the caller
/// writes to the socket).
///
/// Option-specific handling:
/// - `DO ECHO` → accept with `WILL ECHO` (we want server echo so typed
///   characters appear).
/// - `DO SUPPRESS_GO_AHEAD` → accept with `WILL SUPPRESS_GO_AHEAD`.
/// - `DO TERMINAL_TYPE` → accept with `WILL TERMINAL_TYPE`; when the server
///   follows up with `SB TERMINAL_TYPE SEND IAC SE`, reply with our
///   terminal type string.
/// - `DO NAWS` → accept with `WILL NAWS` (size sent on connect + resize).
/// - All others → refuse (safe NVT default).
fn process_iac(chunk: &[u8], state: &mut IacState, data_out: &mut Vec<u8>, neg_out: &mut Vec<u8>) {
    let mut byt = chunk.iter().copied();

    // Track the first byte of a subnegotiation body (the option code) so we
    // can distinguish TERMINAL_TYPE subnegotiations from others.
    let mut sb_opt: Option<u8> = None;

    while let Some(b) = byt.next() {
        match *state {
            IacState::Normal => {
                if b == IAC {
                    *state = IacState::Iac;
                } else {
                    data_out.push(b);
                }
            }
            IacState::Iac => match b {
                IAC => {
                    data_out.push(IAC);
                    *state = IacState::Normal;
                }
                WILL => *state = IacState::Will,
                WONT => *state = IacState::Wont,
                DO => *state = IacState::Do,
                DONT => *state = IacState::Dont,
                SB => {
                    *state = IacState::Sb;
                    sb_opt = None;
                }
                _ => *state = IacState::Normal,
            },
            IacState::Will => {
                // Server offers `b`. Accept ECHO + SUPPRESS_GO_AHEAD; refuse
                // everything else.
                if b == opt::ECHO || b == opt::SUPPRESS_GO_AHEAD {
                    neg_out.extend_from_slice(&[IAC, DO, b]);
                } else {
                    neg_out.extend_from_slice(&[IAC, DONT, b]);
                }
                *state = IacState::Normal;
            }
            IacState::Wont => {
                *state = IacState::Normal;
            }
            IacState::Do => {
                // Server asks us to enable `b`. Accept ECHO, SUPPRESS_GA,
                // NAWS, TERMINAL_TYPE; refuse everything else.
                if b == opt::ECHO
                    || b == opt::SUPPRESS_GO_AHEAD
                    || b == opt::NAWS
                    || b == opt::TERMINAL_TYPE
                {
                    neg_out.extend_from_slice(&[IAC, WILL, b]);
                } else {
                    neg_out.extend_from_slice(&[IAC, WONT, b]);
                }
                *state = IacState::Normal;
            }
            IacState::Dont => {
                *state = IacState::Normal;
            }
            IacState::Sb => {
                if b == IAC {
                    *state = IacState::SbIac;
                } else if sb_opt.is_none() {
                    sb_opt = Some(b);
                }
            }
            IacState::SbIac => match b {
                SE => {
                    // Subnegotiation ended. If the server asked for our
                    // terminal type (SB TERMINAL_TYPE SEND), reply with
                    // our TERM env var.
                    if sb_opt == Some(opt::TERMINAL_TYPE) {
                        let term =
                            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
                        let mut reply = vec![IAC, SB, opt::TERMINAL_TYPE, 0]; // 0 = IS
                        reply.extend_from_slice(term.as_bytes());
                        reply.extend_from_slice(&[IAC, SE]);
                        neg_out.extend_from_slice(&reply);
                    }
                    *state = IacState::Normal;
                    sb_opt = None;
                }
                IAC => {
                    *state = IacState::Sb;
                }
                _ => *state = IacState::Sb,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// CrabPortTerminal impl
// ---------------------------------------------------------------------------

impl CrabPortTerminal for TelnetBackend {
    fn write(&self, data: &[u8]) {
        let _ = self.command_tx.try_send(Command::Write(data.to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.command_tx.try_send(Command::Resize(cols, rows));
    }

    fn close(&self) {
        let _ = self.command_tx.try_send(Command::Close);
    }

    fn subscribe(&self) -> async_broadcast::Receiver<BackendEvent> {
        self.event_tx.new_receiver()
    }

    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        Some(self)
    }

    fn allow_sftp(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// CrabPortMonitor impl
// ---------------------------------------------------------------------------

impl CrabPortMonitor for TelnetBackend {
    fn status(&self) -> RemoteStatus {
        self.monitor.read().status
    }

    fn metrics(&self) -> RemoteMetrics {
        self.monitor.read().metrics
    }
}

impl Drop for TelnetBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a chunk through process_iac, returning (visible_data, negotiation_bytes).
    fn parse(chunk: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut state = IacState::Normal;
        let mut data_out = Vec::new();
        let mut neg_out = Vec::new();
        process_iac(chunk, &mut state, &mut data_out, &mut neg_out);
        (data_out, neg_out)
    }

    /// Parse and return only the visible data.
    fn parse_data(chunk: &[u8]) -> Vec<u8> {
        parse(chunk).0
    }

    #[test]
    fn plain_data_passes_through() {
        let data = parse_data(b"hello\r\n");
        assert_eq!(data, b"hello\r\n");
    }

    #[test]
    fn escaped_iac_yields_literal_255() {
        let data = parse_data(&[IAC, IAC, b'x']);
        assert_eq!(data, vec![IAC, b'x']);
    }

    #[test]
    fn do_echo_accepted_with_will() {
        let (data, neg) = parse(&[IAC, DO, opt::ECHO]);
        assert_eq!(data, b"");
        assert_eq!(neg, vec![IAC, WILL, opt::ECHO]);
    }

    #[test]
    fn do_naws_accepted_with_will() {
        let (data, neg) = parse(&[IAC, DO, opt::NAWS]);
        assert_eq!(data, b"");
        assert_eq!(neg, vec![IAC, WILL, opt::NAWS]);
    }

    #[test]
    fn do_unknown_refused_with_wont() {
        let (data, neg) = parse(&[IAC, DO, 99]);
        assert_eq!(data, b"");
        assert_eq!(neg, vec![IAC, WONT, 99]);
    }

    #[test]
    fn will_echo_accepted_with_do() {
        let (data, neg) = parse(&[IAC, WILL, opt::ECHO]);
        assert_eq!(data, b"");
        assert_eq!(neg, vec![IAC, DO, opt::ECHO]);
    }

    #[test]
    fn will_unknown_refused_with_dont() {
        let (data, neg) = parse(&[IAC, WILL, 99]);
        assert_eq!(data, b"");
        assert_eq!(neg, vec![IAC, DONT, 99]);
    }

    #[test]
    fn wont_and_dont_produce_no_reply() {
        let (data, neg) = parse(&[IAC, WONT, 3, IAC, DONT, 5]);
        assert_eq!(data, b"");
        assert_eq!(neg, b"");
    }

    #[test]
    fn subnegotiation_is_skipped() {
        let (data, neg) = parse(&[
            b'a', b'b', // visible
            IAC, SB, 31, 0, 80, 0, 24, IAC, SE, // NAWS subneg (not TERMINAL_TYPE)
            b'c',
        ]);
        assert_eq!(data, b"abc");
        assert_eq!(neg, b"");
    }

    #[test]
    fn iac_split_across_chunks() {
        let mut state = IacState::Normal;
        let mut data = Vec::new();
        let mut neg = Vec::new();
        process_iac(&[b'x', IAC], &mut state, &mut data, &mut neg);
        process_iac(&[DO, 24], &mut state, &mut data, &mut neg);
        assert_eq!(data, b"x");
        assert_eq!(neg, vec![IAC, WILL, 24]);
    }

    #[test]
    fn auto_login_detects_login_prompt() {
        assert!(has_login_prompt(b"\r\nlogin: "));
        assert!(has_login_prompt(b"Login: "));
        assert!(has_login_prompt(b"Ubuntu 22.04 login: "));
        assert!(!has_login_prompt(b"Welcome!"));
    }

    #[test]
    fn auto_login_detects_password_prompt() {
        assert!(has_password_prompt(b"\r\nPassword: "));
        assert!(has_password_prompt(b"password: "));
        assert!(!has_password_prompt(b"Enter password"));
    }

    #[test]
    fn auto_login_phase_progression() {
        let info = TelnetConnectionInfo::new("host", "admin", "secret");
        let mut al = AutoLogin::new();
        // Idle → LoginPrompt
        let r = al.feed(b"\r\nlogin: ", &info);
        assert!(r.is_some());
        assert_eq!(al.phase, LoginPhase::LoginPrompt);
        // LoginPrompt → PasswordPrompt
        let r = al.feed(b"\r\nPassword: ", &info);
        assert!(r.is_some());
        assert_eq!(al.phase, LoginPhase::PasswordPrompt);
        // After password, no more sends
        let r = al.feed(b"Welcome!", &info);
        assert!(r.is_none());
        assert_eq!(al.phase, LoginPhase::Done);
    }
}
