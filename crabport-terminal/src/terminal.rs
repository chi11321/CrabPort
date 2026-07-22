use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    sync::FairMutex,
    term::{Config, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};
use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};
use parking_lot::Mutex;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Data(Vec<u8>),
    Closed,
    Error(String),
    /// A file transfer (download or upload) finished.
    ///
    /// `kind` identifies which direction, `success` is true on completion
    /// and false on failure, and `message` is a short human-readable
    /// description (the destination path on success, the error text on
    /// failure).
    SftpTransferFinished {
        kind: SftpTransferKind,
        success: bool,
        message: String,
    },
    /// Live progress for an in-flight SFTP transfer. Emitted at each stage
    /// boundary of the gzip/tmp staging flow (compress → transfer →
    /// decompress → cleanup) so the UI can surface a stage-aware progress
    /// log. A `SftpTransferFinished` always follows the last progress
    /// event for a given transfer.
    SftpTransferProgress {
        kind: SftpTransferKind,
        stage: SftpTransferStage,
        /// Short human-readable detail — typically the path being worked
        /// on, so the user can tell which file in a batch is current.
        message: String,
        /// Byte-level progress for the current stage. `None` for stages
        /// that don't have a meaningful byte count (e.g. remote `gzip`
        /// which runs as an opaque exec). When present, the UI renders a
        /// determinate progress bar.
        bytes: Option<SftpTransferBytes>,
    },
    /// Shell history loaded from the TTY's history file
    /// (`~/.bash_history`, `~/.zsh_history`, …). Emitted by a backend in
    /// response to [`CrabPortTerminal::refresh_history`]; the payload is
    /// most-recent-first (already reversed by the backend) so the UI can
    /// render it verbatim.
    HistoryLoaded(Vec<String>),
    /// The shell's current working directory and/or foreground process
    /// changed. Emitted by the local PTY backend's process watcher (which
    /// uses `tcgetpgrp` + `sysinfo` to inspect the foreground process
    /// group on every PTY data event — no shell integration required).
    /// The UI uses this to update the tab title to reflect the live cwd
    /// and shell name (e.g. `Downloads-zsh`). The process name is the
    /// basename of the foreground process's executable (e.g. `zsh`,
    /// `bash`, `python`) — when the user runs `bash` inside a zsh tab,
    /// the title updates to reflect the new shell.
    ProcessChanged {
        cwd: std::path::PathBuf,
        process_name: String,
    },
    /// The backend finished its async construction and is ready to
    /// read/write. Emitted by [`crate::pty::PendingPtyBackend`] once the
    /// real `PtyBackend` has been installed on the worker thread.
    ///
    /// This is needed on Windows where `PtyBackend::new` runs on a
    /// background thread (200–500 ms for `CreatePseudoConsole` + spawning
    /// `pwsh.exe`). Without an explicit signal, the UI would keep the
    /// "Connecting" spinner running at ~120 Hz until the first PTY byte
    /// arrives — which for PowerShell can be another second or more
    /// after the backend itself is ready. Broadcasting `Ready` lets the
    /// UI check `monitor().status()` immediately and flip to `Local`,
    /// stopping the spinner pump as soon as the backend is usable.
    Ready,
}

/// Byte-level progress snapshot for a transfer stage.
#[derive(Debug, Clone, Copy)]
pub struct SftpTransferBytes {
    /// Bytes processed so far in the current stage.
    pub done: u64,
    /// Total bytes expected for the current stage. Zero means "unknown";
    /// the UI should render an indeterminate (animated) bar in that case.
    pub total: u64,
}

/// Which direction an SFTP transfer ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpTransferKind {
    Download,
    Upload,
    /// A rename / move operation (no actual byte transfer, but we reuse the
    /// finished event so the UI's existing plumbing applies).
    Rename,
    /// "Open in editor": download → local edit → re-upload on save. Success
    /// is silent; only upload failures surface a notification.
    Edit,
    /// Delete a remote file or directory. Success shows "deleted";
    /// failure shows "delete failed".
    Delete,
    /// Create a directory. Success is silent (the next directory refresh
    /// surfaces the new folder); failure shows "mkdir failed".
    Mkdir,
}

/// A coarse stage in the gzip/tmp staging flow used by SFTP transfers.
///
/// The ordering reflects the typical sequence for a download (compress
/// remotely → stream the .gz down → decompress on the client → clean up
/// the remote tmp); uploads run the mirror image. Not every transfer
/// touches every stage — e.g. a recursive fallback skips compress/
/// decompress and goes straight to per-file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpTransferStage {
    /// Compressing the source (remote `gzip -c` / `tar czf` for downloads,
    /// client-side `tar+gz` for uploads).
    Compress,
    /// Streaming the staged archive over SFTP (download or upload of the
    /// `.gz` / `.tar.gz`).
    Transfer,
    /// Decompressing the staged archive into its final location (client
    /// `tar::Archive::unpack` for downloads, remote `gunzip`/`tar xzf`
    /// for uploads).
    Decompress,
    /// Removing the remote tmp staging file. Best-effort; failures here
    /// don't fail the overall transfer.
    CleanUp,
}

pub trait CrabPortTerminal: Send + Sync {
    fn write(&self, data: &[u8]);
    fn resize(&self, cols: u16, rows: u16);
    fn close(&self);
    fn subscribe(&self) -> BroadcastReceiver<BackendEvent>;

    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        None
    }

    /// Whether this backend supports SFTP.
    fn allow_sftp(&self) -> bool {
        false
    }

    /// Whether this backend supports command-history capture + paste-back.
    /// Defaults to `true` because history lives on `TerminalSession` (not the
    /// backend) and only needs `write`, which every backend implements.
    fn allow_history(&self) -> bool {
        true
    }

    /// Ask the backend to (re)read the shell's history file and broadcast a
    /// [`BackendEvent::HistoryLoaded`] event with the result. Backends that
    /// can read a TTY history file (local PTY via `std::fs`, SSH via exec)
    /// override this; the default implementation is a no-op so backends
    /// without access to a history file (e.g. Telnet) silently keep whatever
    /// the session-captured history already holds.
    ///
    /// Safe to call repeatedly — each call triggers a fresh read.
    fn refresh_history(&self) {}

    /// Whether this backend supports the snippets panel (run / paste via
    /// `write_raw`). Defaults to `true` for the same reason as `allow_history`.
    fn allow_snippets(&self) -> bool {
        true
    }

    /// Whether this backend can lend its connection for borrowed SSH tunnels.
    /// Defaults to `false`; only SSH backends (which implement `CrabPortTunnel`)
    /// override this to `true`.
    fn allow_tunnels(&self) -> bool {
        false
    }

    /// Current SFTP directory entries. Returns None if not yet loaded.
    fn sftp_entries(&self) -> Option<std::sync::Arc<Vec<crabport_sftp::FileEntry>>> {
        None
    }

    /// Current SFTP working directory. Returns None if not yet loaded.
    fn sftp_cwd(&self) -> Option<std::sync::Arc<String>> {
        None
    }

    /// Navigate to a new directory via SFTP. The backend updates entries + cwd
    /// asynchronously and notifies the UI.
    fn sftp_navigate(&self, _path: &str) {}

    /// Download a remote file to `local_path`, using implicit gzip staging
    /// (see `SshBackend::sftp_download`).
    ///
    /// Completion is reported via a [`BackendEvent::SftpTransferFinished`]
    /// event on the backend's event stream — the caller does not need to pass
    /// a callback.
    fn sftp_download(&self, _remote_path: &str, _local_path: &str) {}

    /// Upload `local_path` to `remote_path`, using implicit gzip staging
    /// (see `SshBackend::sftp_upload`). Completion is reported via
    /// [`BackendEvent::SftpTransferFinished`].
    fn sftp_upload(&self, _local_path: &str, _remote_path: &str) {}

    /// Upload multiple files in a single batch. Falls back to per-file upload
    /// if the remote doesn't support tar. Completion is reported via
    /// [`BackendEvent::SftpTransferFinished`] for the batch.
    fn sftp_upload_batch(&self, _items: &[(String, String)]) {}

    /// Create a directory on the remote host at `remote_path`. Parent
    /// directories must already exist (SFTP `mkdir` is not recursive).
    /// Completion is reported via [`BackendEvent::SftpTransferFinished`]
    /// with `kind = Mkdir` (a synthetic kind — there's no actual transfer,
    /// but we reuse the event so the UI's existing finish handling applies).
    fn sftp_mkdir(&self, _remote_path: &str) {}

    /// Delete a remote file or directory at `remote_path`. The backend
    /// stats the path to decide between `remove_file` and `remove_dir`.
    /// Completion is reported via [`BackendEvent::SftpTransferFinished`] with
    /// `kind = Delete` (a synthetic kind — there's no actual transfer, but
    /// we reuse the event so the UI's existing finish handling applies).
    fn sftp_delete(&self, _remote_path: &str) {}

    /// Rename/move a remote file or directory from `old_path` to `new_path`.
    /// The backend resolves `new_path` against the current cwd if it's
    /// relative, and refuses if the destination already exists.
    /// Completion is reported via [`BackendEvent::SftpTransferFinished`].
    fn sftp_rename(&self, _old_path: &str, _new_path: &str) {}

    /// Download a remote file to a local temp path, open it in the OS default
    /// editor, watch for edits, and re-upload on every save until the file is
    /// closed or the backend drops. Completion of the initial download is
    /// reported via [`BackendEvent::SftpTransferFinished`]; subsequent uploads
    /// triggered by saves each emit their own `SftpTransferFinished`.
    fn sftp_open_in_editor(&self, _remote_path: &str) {}

    /// Open a new independent PTY/channel on the *same underlying connection*
    /// and return a new backend driving it. Used by terminal split: each
    /// pane gets its own independent input/output stream without reconnecting.
    ///
    /// - **SSH**: opens a new session channel + PTY + shell on the existing
    ///   authenticated handle (no re-auth, no new TCP connection).
    /// - **Local PTY**: spawns a new shell process.
    /// - **Telnet**: returns `None` (Telnet has no channel multiplexing;
    ///   the caller should create a new connection instead).
    ///
    /// Returns `None` if the backend doesn't support channel spawning, or if
    /// the connection isn't ready yet.
    fn spawn_channel(
        &self,
        _cols: u16,
        _rows: u16,
    ) -> Option<std::sync::Arc<dyn CrabPortTerminal>> {
        None
    }
}

// ---------------------------------------------------------------------------
// Remote performance monitoring
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoteStatus {
    Local,
    Connected,
    Connecting,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkStats {
    pub bytes_sent: u64,
    pub bytes_recv: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStats {
    pub total: u64,
    pub used: u64,
}

/// CPU utilization snapshot. `usage_pct` is the aggregate across all
/// logical cores (0.0–100.0).
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuStats {
    /// Aggregate CPU usage percentage across all cores, 0.0–100.0.
    pub usage_pct: f32,
}

/// Disk utilization snapshot. `used` / `total` are bytes on the *primary*
/// disk (the one the OS boots from on the local machine, or `/` on the
/// remote). We intentionally do not surface every mount: a typical user
/// only cares about "how full is my main drive", and enumerating every
/// mount on a server with many volumes would make the chip unreadable
/// and slow to query.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskStats {
    pub total: u64,
    pub used: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteMetrics {
    pub latency_ms: Option<u32>,
    pub memory: Option<MemoryStats>,
    pub network: Option<NetworkStats>,
    pub cpu: Option<CpuStats>,
    pub disk: Option<DiskStats>,
}

pub trait CrabPortMonitor: Send + Sync {
    fn status(&self) -> RemoteStatus;
    fn metrics(&self) -> RemoteMetrics;
}

#[derive(Clone)]
pub struct EventProxy {
    wakeup_tx: BroadcastSender<()>,
}

impl EventProxy {
    pub fn new(wakeup_tx: BroadcastSender<()>) -> Self {
        Self { wakeup_tx }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                tracing::debug!("EventProxy: Wakeup event received");
                let _ = self.wakeup_tx.try_broadcast(());
            }
            _ => {
                tracing::debug!("EventProxy: Other event {:?}", event);
                let _ = self.wakeup_tx.try_broadcast(());
            }
        }
    }
}

/// Maximum number of commands retained per session. Matches the Store
/// limit so the in-memory buffer and the persisted table stay in sync.
/// Older entries are dropped once this limit is exceeded (LRU by most
/// recent use).
const MAX_COMMAND_HISTORY: usize = 300;

/// State of the input-stream ANSI escape parser used by
/// [`TerminalSession::capture_command`].
///
/// Arrow keys, Home/End, Delete, PageUp/Down, etc. emit multi-byte
/// sequences (`ESC [ A` for ↑, `ESC [ C` for →, `ESC O c` for some
/// keypads, …). Their printable tail (`[A`, `[C`, …) must NOT leak into
/// the command buffer, so we run a tiny state machine that skips the
/// whole sequence. The state is persisted across `write` calls because a
/// single key press may arrive split across multiple packets.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CaptureState {
    #[default]
    Normal,
    /// Saw `ESC` — waiting for the next byte to tell us what kind of
    /// sequence this is.
    Esc,
    /// Inside a CSI (`ESC [`) or SS3 (`ESC O`) sequence, waiting for the
    /// final byte (0x40..=0x7e). Parameter/intermediate bytes
    /// (0x20..=0x3f) keep us in this state.
    AwaitFinal,
}

pub struct TerminalSession {
    backend: Arc<dyn CrabPortTerminal>,
    term: Arc<FairMutex<Term<EventProxy>>>,
    wakeup_tx: BroadcastSender<()>,
    started: AtomicBool,
    _wakeup_rx: InactiveReceiver<()>,
    /// Command history, most-recent-first. Capped at [`MAX_COMMAND_HISTORY`]
    /// entries; the oldest is evicted when full.
    ///
    /// Shared across all split panes of the same tab so the History panel
    /// stays consistent no matter which pane is active. Each split pane
    /// clones this `Arc` from its source pane via
    /// [`TerminalSession::new_with_shared_history`].
    command_history: Arc<Mutex<VecDeque<String>>>,
    /// In-progress input line + ANSI escape parser state, accumulated by
    /// [`Self::write`] and submitted to `command_history` on Enter (CR/LF).
    /// Backspace deletes the last char; ANSI escape sequences (arrow keys,
    /// Home/End, Delete, …) are skipped by the [`CaptureState`] machine so
    /// their printable tail (`[A`, `[C`, …) never pollutes the buffer.
    line_buffer: Arc<Mutex<(String, CaptureState)>>,
    /// Optional callback invoked whenever a new command is captured. The UI
    /// layer (TerminalView) uses this to persist the command to the Store
    /// — TerminalSession itself stays free of any storage dependency.
    /// Receives the captured command text.
    on_command: Arc<Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>>,
}

impl TerminalSession {
    pub fn new(backend: Arc<dyn CrabPortTerminal>, cols: usize, rows: usize) -> Self {
        let (wakeup_tx, wakeup_rx) = broadcast(256);
        let _wakeup_rx = wakeup_rx.deactivate();

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            EventProxy::new(wakeup_tx.clone()),
        )));

        Self {
            backend,
            term,
            wakeup_tx,
            started: AtomicBool::new(false),
            _wakeup_rx,
            command_history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_COMMAND_HISTORY))),
            line_buffer: Arc::new(Mutex::new((String::new(), CaptureState::default()))),
            on_command: Arc::new(Mutex::new(None)),
        }
    }

    /// Like [`new`](Self::new) but shares `command_history` with another
    /// session — used when splitting a terminal pane so all panes of the
    /// same tab see the same command history.
    ///
    /// Each split pane keeps its own `line_buffer` (input-line capture is
    /// per-pane, since each pane has its own prompt); only the completed
    /// history is shared.
    pub fn new_with_shared_history(
        backend: Arc<dyn CrabPortTerminal>,
        cols: usize,
        rows: usize,
        command_history: Arc<Mutex<VecDeque<String>>>,
    ) -> Self {
        let (wakeup_tx, wakeup_rx) = broadcast(256);
        let _wakeup_rx = wakeup_rx.deactivate();

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            EventProxy::new(wakeup_tx.clone()),
        )));

        Self {
            backend,
            term,
            wakeup_tx,
            started: AtomicBool::new(false),
            _wakeup_rx,
            command_history,
            line_buffer: Arc::new(Mutex::new((String::new(), CaptureState::default()))),
            on_command: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let mut rx = self.backend.subscribe();
        let term = self.term.clone();
        let wakeup_tx = self.wakeup_tx.clone();
        let command_history = self.command_history.clone();

        smol::spawn(async move {
            let mut parser = Processor::<StdSyncHandler>::new();

            loop {
                match rx.recv().await {
                    Ok(event) => match event {
                        BackendEvent::Data(data) => {
                            tracing::debug!("session: received {} bytes", data.len());
                            // Batch-drain: hold the term lock once and advance all
                            // currently-queued chunks. Cuts lock churn and wakeup
                            // storms when the PTY floods (cat / top / build logs).
                            let mut terminal = term.lock();
                            parser.advance(&mut *terminal, &data);
                            loop {
                                match rx.try_recv() {
                                    Ok(BackendEvent::Data(more)) => {
                                        parser.advance(&mut *terminal, &more);
                                    }
                                    Ok(BackendEvent::Closed) => {
                                        drop(terminal);
                                        let _ = wakeup_tx.try_broadcast(());
                                        return;
                                    }
                                    Ok(BackendEvent::Error(err)) => {
                                        tracing::error!("terminal backend error: {}", err);
                                    }
                                    Ok(BackendEvent::SftpTransferFinished { .. }) => {
                                        // UI-only event; ignore during batch drain.
                                    }
                                    Ok(BackendEvent::SftpTransferProgress { .. }) => {
                                        // UI-only event; ignore during batch drain.
                                    }
                                    Ok(BackendEvent::HistoryLoaded(_)) => {
                                        // Handled in the outer loop (not here,
                                        // inside the term lock) — drain ignores it.
                                    }
                                    Ok(BackendEvent::ProcessChanged { .. }) => {
                                        // UI-only event; ignore during batch drain.
                                    }
                                    Ok(BackendEvent::Ready) => {
                                        // UI-only event; ignore during
                                        // batch drain. The outer match
                                        // handles it (wakeup → status
                                        // check).
                                    }
                                    Err(_) => break, // queue drained
                                }
                            }
                            drop(terminal);
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        BackendEvent::Closed => {
                            tracing::info!("session: backend closed");
                            let _ = wakeup_tx.try_broadcast(());
                            break;
                        }
                        BackendEvent::Error(err) => {
                            tracing::error!("terminal backend error: {}", err);
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        // Transfer-finished events are for the UI, not the
                        // terminal parser. Ignore them here.
                        BackendEvent::SftpTransferFinished { .. } => {}
                        BackendEvent::SftpTransferProgress { .. } => {}
                        // Replace the in-memory command history with the
                        // freshly-loaded TTY history file contents. The
                        // backend delivers most-recent-first, so we assign
                        // the deque front-to-back as given. A wake-up is
                        // broadcast so the UI repaints with the new list.
                        BackendEvent::HistoryLoaded(cmds) => {
                            let mut history = command_history.lock();
                            history.clear();
                            for c in cmds {
                                history.push_back(c);
                            }
                            drop(history);
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        // Cwd change: the session doesn't need to do
                        // anything — the `TerminalView` listens on the same
                        // backend event stream and updates the tab title.
                        // Wake up so any subscriber that cares repaints.
                        BackendEvent::ProcessChanged { .. } => {
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        // Backend finished async construction (e.g.
                        // `PendingPtyBackend` installed the real `PtyBackend`).
                        // The session itself has nothing to do — the UI's
                        // wakeup listener re-reads `monitor().status()` and
                        // flips the overlay out of the "Connecting" spinner.
                        // Without this, the spinner would keep running at
                        // ~120 Hz until the first PTY `Data` byte arrives,
                        // which on Windows PowerShell can be a second or more
                        // after the backend is already usable.
                        BackendEvent::Ready => {
                            let _ = wakeup_tx.try_broadcast(());
                        }
                    },
                    Err(_e) => {
                        tracing::warn!("session: recv error: {:?}", _e);
                        let _ = wakeup_tx.try_broadcast(());
                        break;
                    }
                }
            }
        })
        .detach();
    }

    pub fn with_term<R>(&self, f: impl FnOnce(&Term<EventProxy>) -> R) -> R {
        let term = self.term.lock();
        f(&*term)
    }

    /// Mutable access — needed to read & reset alacritty damage.
    pub fn with_term_mut<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> R {
        let mut term = self.term.lock();
        f(&mut *term)
    }

    /// Non-blocking mutable access. Returns `None` if the reader thread currently
    /// holds the lock — the caller should reuse the previous frame's snapshot
    /// instead of stalling the render thread.
    pub fn try_with_term_mut<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> Option<R> {
        self.term.try_lock_unfair().map(|mut t| f(&mut *t))
    }

    pub fn feed_escape(&self, data: &[u8]) {
        let mut term = self.term.lock();
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut *term, data);
    }

    pub fn write(&self, data: &[u8]) {
        self.capture_command(data);
        self.backend.write(data);
    }

    /// Write raw bytes to the backend **without** capturing them into the
    /// command history. Used by the History panel's "paste" action so
    /// inserting a historical command into the input line doesn't re-record
    /// it as a new entry.
    pub fn write_raw(&self, data: &[u8]) {
        self.backend.write(data);
    }

    /// Snapshot of the command history, most-recent-first. Cheap clone —
    /// the caller typically hands this to a UI panel each render.
    pub fn command_history(&self) -> Vec<String> {
        self.command_history.lock().iter().cloned().collect()
    }

    /// Direct mutable access to the underlying history deque. Used by the
    /// UI layer to pre-seed the in-memory buffer with persisted history on
    /// session creation. Returns a guard; caller assigns the whole deque.
    pub fn command_history_deque(&self) -> parking_lot::MutexGuard<'_, VecDeque<String>> {
        self.command_history.lock()
    }

    /// Cloned handle to the shared command-history buffer. Used when
    /// splitting a pane so the new pane shares the same history as its
    /// source (see [`Self::new_with_shared_history`]).
    pub fn command_history_arc(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.command_history.clone()
    }

    /// Register a callback invoked whenever a new command is captured
    /// (submitted via Enter). The UI layer uses this to persist commands
    /// to the Store — `TerminalSession` itself has no storage dependency.
    /// Pass `None` to clear a previously-set callback.
    pub fn set_on_command(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        *self.on_command.lock() = cb;
    }

    /// Best-effort command capture from the raw input stream.
    ///
    /// Runs a small state machine (see [`CaptureState`]) over the bytes so
    /// that ANSI escape sequences emitted by editing keys — arrow keys
    /// (`ESC [ A/B/C/D`), Home/End, Delete, PageUp/Down, etc. — are skipped
    /// in full instead of leaking their printable tail (`[A`, `[C`, …) into
    /// the buffer. The state persists across `write` calls because a single
    /// key press may arrive split across multiple packets.
    ///
    /// Printable ASCII (`0x20..=0x7e`) and UTF-8 multibyte bytes
    /// (`0x80..=0xff`) are appended; Backspace (DEL `0x7f` / BS `0x08`)
    /// deletes the last char; CR/LF submits the buffer. Empty results and
    /// exact-duplicates of the most recent entry are skipped.
    ///
    /// This is intentionally simple — it doesn't mirror the PTY's idea of
    /// the current line, so commands recalled with `↑` and then edited
    /// won't be captured perfectly (the buffer only reflects what the user
    /// typed in this session, not what readline echoed back). But it's
    /// accurate for the common typed-and-Enter case, and the cost (a couple
    /// of locks + a small alloc) is negligible.
    fn capture_command(&self, data: &[u8]) {
        let mut history = self.command_history.lock();
        let mut line = self.line_buffer.lock();
        let (buf, state) = &mut *line;
        for &b in data {
            match *state {
                CaptureState::Normal => match b {
                    // CR or LF — submit the line.
                    0x0d | 0x0a => {
                        let cmd = buf.trim().to_string();
                        buf.clear();
                        if cmd.is_empty() {
                            continue;
                        }
                        // LRU dedup: if the command already exists in
                        // history, remove it from its current position so it
                        // can be re-inserted at the front (most-recently-used).
                        // This mirrors the Store's `updated_at` promotion.
                        if let Some(pos) = history.iter().position(|c| c == &cmd) {
                            history.remove(pos);
                        }
                        if history.len() >= MAX_COMMAND_HISTORY {
                            history.pop_back();
                        }
                        history.push_front(cmd.clone());
                        // Notify the UI layer so it can persist the command.
                        // The callback is cloned out of the Mutex to avoid
                        // calling it while holding the lock.
                        let cb = self.on_command.lock().clone();
                        if let Some(cb) = cb {
                            cb(&cmd);
                        }
                    }
                    // Backspace / DEL — delete the last char.
                    0x08 | 0x7f => {
                        buf.pop();
                    }
                    // ESC — start of an ANSI escape sequence (arrow keys,
                    // Home/End, etc.). Switch to `Esc` and drop this byte so
                    // the sequence's printable tail (`[A`, `[C`, …) never
                    // reaches the buffer.
                    0x1b => {
                        *state = CaptureState::Esc;
                    }
                    // Printable ASCII.
                    0x20..=0x7e => {
                        buf.push(b as char);
                    }
                    // High bytes (UTF-8 continuation / lead) — push raw so
                    // non-ASCII commands aren't lost. We don't validate UTF-8
                    // here; the buffer is only for display, not execution.
                    0x80..=0xff => {
                        buf.push(b as char);
                    }
                    // Other control bytes (Tab, SI/SO, etc.) — ignore.
                    _ => {}
                },
                CaptureState::Esc => match b {
                    // CSI (`ESC [`) or SS3 (`ESC O`) — wait for the final
                    // byte (and any intermediate parameter bytes).
                    b'[' | b'O' => {
                        *state = CaptureState::AwaitFinal;
                    }
                    // Any other byte after ESC is a two-char sequence
                    // (e.g. `ESC =`). Consume it and return to Normal.
                    _ => {
                        *state = CaptureState::Normal;
                    }
                },
                CaptureState::AwaitFinal => match b {
                    // Parameter / intermediate bytes (0x20..=0x3f) — keep
                    // waiting for the final byte.
                    0x20..=0x3f => {}
                    // Final byte (0x40..=0x7e) — sequence complete.
                    0x40..=0x7e => {
                        *state = CaptureState::Normal;
                    }
                    // Unexpected byte — bail back to Normal.
                    _ => {
                        *state = CaptureState::Normal;
                    }
                },
            }
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        {
            let mut term = self.term.lock();
            term.resize(TermSize::new(cols as usize, rows as usize));
        }
        self.backend.resize(cols, rows);
    }

    pub fn close(&self) {
        self.backend.close();
    }

    pub fn subscribe_wakeup(&self) -> BroadcastReceiver<()> {
        self.wakeup_tx.new_receiver()
    }

    pub fn subscribe_backend(&self) -> BroadcastReceiver<BackendEvent> {
        self.backend.subscribe()
    }

    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        self.backend.as_monitor()
    }

    pub fn allow_sftp(&self) -> bool {
        self.backend.allow_sftp()
    }

    pub fn allow_history(&self) -> bool {
        self.backend.allow_history()
    }

    /// Trigger a fresh read of the shell's history file. The backend will
    /// broadcast a [`BackendEvent::HistoryLoaded`] when the data is ready,
    /// which [`TerminalSession::start`] forwards into `command_history`.
    pub fn refresh_history(&self) {
        self.backend.refresh_history();
    }

    pub fn allow_snippets(&self) -> bool {
        self.backend.allow_snippets()
    }

    pub fn allow_tunnels(&self) -> bool {
        self.backend.allow_tunnels()
    }

    pub fn sftp_entries(&self) -> Option<std::sync::Arc<Vec<crabport_sftp::FileEntry>>> {
        self.backend.sftp_entries()
    }

    pub fn sftp_cwd(&self) -> Option<std::sync::Arc<String>> {
        self.backend.sftp_cwd()
    }

    pub fn sftp_navigate(&self, path: &str) {
        self.backend.sftp_navigate(path)
    }

    /// Download a remote file via the implicit-gzip staged flow.
    /// Completion is reported via the backend's event stream as
    /// `BackendEvent::SftpTransferFinished`.
    pub fn sftp_download(&self, remote_path: &str, local_path: &str) {
        self.backend.sftp_download(remote_path, local_path);
    }

    /// Upload a local file via the implicit-gzip staged flow.
    /// Completion is reported via the backend's event stream as
    /// `BackendEvent::SftpTransferFinished`.
    pub fn sftp_upload(&self, local_path: &str, remote_path: &str) {
        self.backend.sftp_upload(local_path, remote_path);
    }

    /// Upload multiple files in a single batch transfer.
    /// Completion is reported via the backend's event stream as
    /// `BackendEvent::SftpTransferFinished`.
    pub fn sftp_upload_batch(&self, items: &[(String, String)]) {
        self.backend.sftp_upload_batch(items);
    }

    /// Delete a remote file or directory.
    /// Completion is reported via the backend's event stream as
    /// `BackendEvent::SftpTransferFinished`.
    pub fn sftp_delete(&self, remote_path: &str) {
        self.backend.sftp_delete(remote_path);
    }

    /// Create a directory on the remote host (non-recursive — parent must
    /// exist). Completion is reported via the backend's event stream as
    /// `BackendEvent::SftpTransferFinished` with `kind = Mkdir`.
    pub fn sftp_mkdir(&self, remote_path: &str) {
        self.backend.sftp_mkdir(remote_path);
    }

    pub fn scroll(&self, delta: i32) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        term.scroll_display(Scroll::Delta(delta));
        let _ = self.wakeup_tx.try_broadcast(());
    }

    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        term.scroll_display(Scroll::Bottom);
        let _ = self.wakeup_tx.try_broadcast(());
    }
}
