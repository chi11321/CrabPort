use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    index::{Column, Line, Point as AlacPoint},
    sync::FairMutex,
    term::{Config, TermMode, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};
use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};
use parking_lot::Mutex;
use std::collections::VecDeque;

use alacritty_terminal::index::Direction as AlacDirection;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};

/// A single search match in the terminal grid, expressed as grid absolute
/// coordinates. A match spans from `(start_line, start_col)` to
/// `(end_line, end_col)` inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_line: i32,
    pub start_col: usize,
    pub end_line: i32,
    pub end_col: usize,
}

impl SearchMatch {
    fn from_alacritty(range: std::ops::RangeInclusive<AlacPoint>) -> Self {
        let start = range.start();
        let end = range.end();
        Self {
            start_line: start.line.0,
            start_col: start.column.0,
            end_line: end.line.0,
            end_col: end.column.0,
        }
    }
}

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

/// Strip a leading prompt marker from a row that was read back from the
/// alacritty grid, returning the command text that follows it.
///
/// Three prompt shapes are handled (best-effort heuristic — see the
/// caveats below):
///
/// 1. **Simple leading marker** — the row starts with one of `$`, `%`,
///    `#`, `>`, `❯`, `➜` (optionally followed by a single space). The
///    marker is stripped.
/// 2. **Complex PS1 with trailing-space marker** —
///    `alice@box:~/repo$ ls -l` / `~/repo % ls -l`. We find the *last*
///    occurrence of `<marker><space>` and trim everything up to it,
///    requiring the prefix to look like a real PS1 (contains one of `@`,
///    `:`, `/`, `~`) so a `$` mid-command doesn't cause a false cut.
/// 3. **Complex PS1 with no trailing space** — this is the case the
///    original heuristic missed and which caused
///    `weed@MacBook-Pro ~ %` (macOS zsh default PS1, `%#` has no trailing
///    space) to leak into the history verbatim. We additionally try the
///    *last* occurrence of a single marker char (no trailing space),
///    again gated by the PS1-indicator check on the prefix. This also
///    handles prompt-only lines: `weed@MacBook-Pro ~ %` strips to `""`,
///    and `snapshot_command_from_grid` drops empty results.
///
/// Returns the trimmed remainder (possibly empty). If no known prompt
/// marker is found, the input is returned unchanged (trimmed).
///
/// Caveats: a command containing a bare `$` / `%` / `#` after a path-like
/// prefix (e.g. `echo ~/foo $ bar`) can still be over-trimmed. The
/// `refresh_history` path re-syncs from the shell's own history file
/// (which never records the prompt) as ground truth, so any transient
/// misread is corrected on the next refresh.
fn strip_prompt_prefix(line: &str) -> &str {
    // Simple leading marker: `$ cmd`, `% cmd`, ...
    let head = line.trim_start();
    for marker in [
        "$ ", "% ", "# ", "> ", "❯ ", "➜ ", "$", "%", "#", ">", "❯", "➜",
    ] {
        if let Some(rest) = head.strip_prefix(marker) {
            return rest.trim_start();
        }
    }

    // Heuristic test for "the prefix before this marker looks like a shell
    // PS1" — must contain at least one of `@`, `:`, `/`, `~`. Without this
    // gate we'd happily cut `echo $FOO` at `$` and return `FOO`.
    fn prefix_looks_like_ps1(prefix: &str) -> bool {
        prefix.contains('@') || prefix.contains(':') || prefix.contains('/') || prefix.contains('~')
    }

    // Complex PS1 with `<marker><space>`, e.g.
    //   `alice@box:~/repo$ ls -l`
    //   `~/repo % ls -l`
    for marker in ["$ ", "# ", "% "] {
        if let Some(idx) = line.rfind(marker) {
            let prefix = &line[..idx];
            if prefix_looks_like_ps1(prefix) {
                return line[idx + marker.len()..].trim();
            }
        }
    }

    // Complex PS1 with a *bare* trailing marker (no following space), e.g.
    //   `weed@MacBook-Pro ~ %ls`     (zsh `%#`, user typed `ls` no space)
    //   `weed@MacBook-Pro ~ %`         (zsh prompt-only line → empty)
    //   `alice@box:~/repo$ls`         (bash, no space)
    // Try each marker char in turn; the markers that double as shell sigils
    // (`$`, `%`, `#`) are risky so the PS1-indicator gate is essential.
    // Marker is a single `char`, so the byte length matches `char::len_utf8()`.
    for marker in ['%', '$', '#', '❯', '➜'] {
        if let Some(idx) = line.rfind(marker) {
            let prefix = &line[..idx];
            if prefix_looks_like_ps1(prefix) {
                let rest = &line[idx + marker.len_utf8()..];
                // Allow an optional single space right after the marker
                // (covers `weed@…~ % ls` written with a space too).
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                return rest.trim();
            }
        }
    }

    line.trim()
}

/// Merge freshly-loaded shell-history file entries into the in-memory
/// deque.
///
/// `cmds` is **newest-first** (the backends `reverse()` the file before
/// broadcasting — see `crabport_ssh::terminal::parse_shell_history` and
/// `crabport_terminal::pty::parse_local_shell_history`). The merge keeps
/// the deque ordered newest-at-front:
///
/// - Entries that are **only in memory** (not in the file) are kept at the
///   front. These are the Enter-triggered grid snapshots of commands the
///   shell hasn't flushed to its history file yet (default-config bash
///   only writes `~/.bash_history` on exit) — they're newer than anything
///   the file can tell us, and dropping them was part of the
///   "history entries vanish right after arriving" bug. (Store pre-seed
///   leftovers are also caught here — they're older, but harmless and
///   rare; keeping them beats losing data.)
/// - File-only entries are appended after them, in the file's
///   newest-first order.
///
/// The deque is capped at [`MAX_COMMAND_HISTORY`] entries, evicting from
/// the tail (the oldest). Because file entries are appended at the tail,
/// a full deque drops the *file's oldest* entries — never the fresh
/// in-memory snapshots at the front.
fn merge_history_entries(history: &mut VecDeque<String>, cmds: &[String]) {
    use std::collections::HashSet;

    let file_set: HashSet<&str> = cmds.iter().map(String::as_str).collect();
    // Memory-only entries keep their current (newest-first) order and go
    // to the front of the merged list.
    let memory_only: Vec<String> = history
        .iter()
        .filter(|e| !file_set.contains(e.as_str()))
        .cloned()
        .collect();

    history.clear();
    let mut kept: HashSet<String> = memory_only.iter().cloned().collect();
    for e in memory_only {
        history.push_back(e);
    }
    for c in cmds {
        if !kept.insert(c.clone()) {
            continue;
        }
        history.push_back(c.clone());
        if history.len() > MAX_COMMAND_HISTORY {
            history.pop_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{MAX_COMMAND_HISTORY, merge_history_entries, strip_prompt_prefix};

    // Regression for issue #69: macOS zsh default PS1 `%n@%m %1~ %#`
    // renders as `weed@MacBook-Pro ~ %` — no trailing space after the
    // marker. The original heuristic only looked for `<marker><space>`
    // and missed this form, dumping the entire prompt line into the
    // history sidebar.
    #[test]
    fn strip_prompt_handles_macos_zsh_default_ps1() {
        // Empty prompt line (just the PS1, user pressed Enter on an empty
        // input) — must strip to empty so `snapshot_command_from_grid`
        // drops it.
        assert_eq!(strip_prompt_prefix("weed@MacBook-Pro ~ %"), "");
        // User typed `ls` without a space after `%`.
        assert_eq!(strip_prompt_prefix("weed@MacBook-Pro ~ %ls"), "ls");
        // User typed `ls` with a space after `%`.
        assert_eq!(strip_prompt_prefix("weed@MacBook-Pro ~ % ls"), "ls");
    }

    #[test]
    fn strip_prompt_handles_bash_default_ps1() {
        // `\u@\h:\w$ ` style
        assert_eq!(strip_prompt_prefix("alice@box:~/repo$ ls -l"), "ls -l");
        assert_eq!(strip_prompt_prefix("alice@box:~/repo$ls"), "ls");
        assert_eq!(strip_prompt_prefix("alice@box:~/repo$ "), "");
    }

    #[test]
    fn strip_prompt_handles_simple_leading_markers() {
        assert_eq!(strip_prompt_prefix("$ ls"), "ls");
        assert_eq!(strip_prompt_prefix("% ls"), "ls");
        assert_eq!(strip_prompt_prefix("# ls"), "ls");
        assert_eq!(strip_prompt_prefix("> ls"), "ls");
        assert_eq!(strip_prompt_prefix("❯ ls"), "ls");
        assert_eq!(strip_prompt_prefix("➜ ls"), "ls");
    }

    #[test]
    fn strip_prompt_does_not_overtrim_commands_with_sigils() {
        // `$` / `%` / `#` mid-command without a PS1-shaped prefix must NOT
        // be cut — the whole line is the command.
        assert_eq!(strip_prompt_prefix("echo $FOO"), "echo $FOO");
        assert_eq!(
            strip_prompt_prefix("echo # not a comment"),
            "echo # not a comment"
        );
        assert_eq!(strip_prompt_prefix("echo 50%"), "echo 50%");
        assert_eq!(strip_prompt_prefix("echo ~/foo"), "echo ~/foo");
        // Known false-positive (unavoidable with the heuristic): a `$` /
        // `%` placed right after a path-like prefix in the *command body*
        // is over-trimmed. We document this with a test so any future tweak
        // is forced to make a deliberate call either way. The
        // `refresh_history` resync on the next Enter corrects this in
        // practice for shells that flush their file in real time.
        assert_eq!(strip_prompt_prefix("echo ~/foo $ bar"), "bar");
    }

    // Regression for the "history entries vanish right after arriving" bug:
    // the merge must preserve newest-first order and must not evict
    // in-memory entries that the shell hasn't flushed to its file yet.
    #[test]
    fn merge_history_preserves_newest_first_order_and_keeps_unflushed_entries() {
        // `cmds` is newest-first: ls was the most recent, cd the oldest.
        let cmds = vec!["ls".to_string(), "pwd".to_string(), "cd".to_string()];
        // In-memory already holds `git status` — the snapshot of a command
        // the user just Enter'd, which bash hasn't written to the file yet.
        let mut history: VecDeque<String> = ["git status".to_string()].into();

        merge_history_entries(&mut history, &cmds);

        // Newest first: the unflushed command stays at the front, the file
        // entries follow in file order (newest first), nothing lost.
        let got: Vec<String> = history.into_iter().collect();
        assert_eq!(got, vec!["git status", "ls", "pwd", "cd"]);
    }

    #[test]
    fn merge_history_dedups_and_respects_cap() {
        // A command that exists both in memory and in the file is merged to
        // the file's position; memory-only entries (`pwd`) stay at the
        // front. No duplicates.
        let mut history: VecDeque<String> = ["ls".to_string(), "pwd".to_string()].into();
        let cmds = vec!["ls".to_string(), "vim".to_string()];
        merge_history_entries(&mut history, &cmds);
        let got: Vec<String> = history.into_iter().collect();
        assert_eq!(got, vec!["pwd", "ls", "vim"]);

        // Cap: memory-only entries are never evicted (they're the fresh
        // snapshots), so a full deque drops the *file's oldest* entries
        // instead. `big` holds 298 entries, all memory-only; the 5 file
        // entries fill the remaining 2 slots and the 3 overflow file
        // entries (the newest ones, appended last) are dropped at the tail.
        let mut big: VecDeque<String> = (0..(MAX_COMMAND_HISTORY as i32) - 2)
            .map(|i| format!("old-{i}"))
            .collect();
        let new_cmds: Vec<String> = (0..5).map(|i| format!("new-{i}")).collect();
        merge_history_entries(&mut big, &new_cmds);
        assert_eq!(big.len(), MAX_COMMAND_HISTORY);
        // The oldest memory-only entries survive (never evicted).
        assert!(big.iter().any(|e| e == "old-0"));
        // The two oldest file entries made it in (appended at the tail in
        // file order); the three newest overflow entries were dropped
        // because the deque was already full of memory-only entries.
        assert_eq!(big.back().map(String::as_str), Some("new-1"));
        assert!(!big.iter().any(|e| e == "new-2"));
        assert!(!big.iter().any(|e| e == "new-3"));
        assert!(!big.iter().any(|e| e == "new-4"));
    }
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
    /// Optional callback invoked whenever a new command is captured by
    /// [`Self::snapshot_command_from_grid`] (the Enter-byte-triggered grid
    /// read-back — see [`Self::write`]). The UI layer (TerminalView) uses
    /// this to persist the command to the Store; `TerminalSession` itself
    /// stays free of any storage dependency. Receives the captured command
    /// text.
    on_command: Arc<Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>>,
    /// Last time [`Self::write`] fired [`Self::refresh_history`] as an
    /// Enter-triggered re-sync. Throttles the per-keystroke shell-history
    /// file read so we don't `cat ~/.bash_history` (or run a remote SSH
    /// `cat` over the wire) on every single Enter hit. The backend's own
    /// `Connected` one-shot and the panel's refresh button are independent
    /// of this gate.
    last_history_refresh: Arc<parking_lot::Mutex<std::time::Instant>>,
    /// Kitty keyboard protocol flags negotiated with the program. 0 means
    /// disabled (plain byte input via the IME path). Non-zero bits track
    /// which progressive feature levels are active.
    kitty_keyboard: Arc<AtomicU8>,
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
            on_command: Arc::new(Mutex::new(None)),
            // Pre-date by a minute so the very first Enter always passes the
            // throttle gate (and so a connect-time refresh firing just
            // before doesn't starve the first user Enter).
            last_history_refresh: Arc::new(parking_lot::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            )),
            kitty_keyboard: Arc::new(AtomicU8::new(0)),
        }
    }

    /// Like [`new`](Self::new) but shares `command_history` with another
    /// session — used when splitting a terminal pane so all panes of the
    /// same tab see the same command history.
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
            on_command: Arc::new(Mutex::new(None)),
            last_history_refresh: Arc::new(parking_lot::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            )),
            kitty_keyboard: Arc::new(AtomicU8::new(0)),
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
        let kitty_state = self.kitty_keyboard.clone();

        smol::spawn(async move {
            let mut parser = Processor::<StdSyncHandler>::new();

            loop {
                match rx.recv().await {
                    Ok(event) => match event {
                        BackendEvent::Data(data) => {
                            tracing::debug!("session: received {} bytes", data.len());
                            // Kitty keyboard protocol *negotiation*: the program
                            // (e.g. a TUI like opencode) asks the terminal to
                            // switch to `CSI u` key reporting. We only enable our
                            // CSI u *output* encoding after the program explicitly
                            // requests it, so plain shells keep receiving ordinary
                            // key bytes and we never inject reply bytes into the
                            // stream (which a multiplexer like tmux would forward
                            // to the pane and print as garbage).
                            Self::scan_kitty_negotiation(&data, &kitty_state);
                            // Batch-drain: hold the term lock once and advance all
                            // currently-queued chunks. Cuts lock churn and wakeup
                            // storms when the PTY floods (cat / top / build logs).
                            let mut terminal = term.lock();
                            parser.advance(&mut *terminal, &data);
                            // Implicit kitty-keyboard disable: if kitty is active
                            // but the program has left the alternate screen (e.g.
                            // opencode exiting its TUI without sending `ESC[<u`),
                            // stop encoding keys as `CSI u` so the plain shell that
                            // follows does not receive garbage. Querying the real
                            // alt-screen mode is far more reliable than guessing at
                            // the `?1049l` byte sequence in the stream.
                            if kitty_state.load(Ordering::SeqCst) != 0
                                && !terminal.mode().contains(TermMode::ALT_SCREEN)
                            {
                                kitty_state.store(0, Ordering::SeqCst);
                                tracing::debug!(
                                    "kitty keyboard: disabled because alt-screen exited (mode {:?})",
                                    terminal.mode()
                                );
                            }
                            loop {
                                match rx.try_recv() {
                                    Ok(BackendEvent::Data(more)) => {
                                        parser.advance(&mut *terminal, &more);
                                        if kitty_state.load(Ordering::SeqCst) != 0
                                            && !terminal.mode().contains(TermMode::ALT_SCREEN)
                                        {
                                            kitty_state.store(0, Ordering::SeqCst);
                                            tracing::debug!(
                                                "kitty keyboard: disabled because alt-screen exited (mode {:?})",
                                                terminal.mode()
                                            );
                                        }
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
                        // Merge the freshly-loaded TTY history file
                        // contents into the in-memory deque. The backend
                        // delivers most-recent-first.
                        //
                        // We *merge* (LRU dedup + promote) instead of
                        // `clear + extend` so that commands the shell has
                        // not yet flushed to its history file (e.g. a
                        // freshly-Enter'd command under default-config bash,
                        // which only writes `~/.bash_history` on shell exit)
                        // aren't evicted by an Enter-triggered resync that
                        // loads an older file snapshot. For zsh users with
                        // `INC_APPEND_HISTORY` / `SHARE_HISTORY` the merge
                        // is a no-op dedup — the just-typed command is
                        // already at the file's front and at the deque's
                        // front, so the entry is just promoted.
                        BackendEvent::HistoryLoaded(cmds) => {
                            // `cmds` is delivered **newest-first** (the
                            // backends `reverse()` the file before
                            // broadcasting). We must preserve that order:
                            // entries already in memory are skipped (no
                            // `remove + push_front`, which would flip the
                            // whole list and shove the freshest command to
                            // the tail — where the cap below would evict
                            // it), and file-only entries are appended at the
                            // tail so the deque stays oldest-at-back.
                            //
                            // Example: file = [ls(newest), pwd, cd], memory
                            // = [git status] (snapshot of a just-Enter'd
                            // command bash hasn't flushed yet) → after merge
                            // deque = [git status, ls, pwd, cd], i.e. the
                            // newest first, nothing lost.
                            let mut history = command_history.lock();
                            merge_history_entries(&mut history, &cmds);
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

    /// Forward user keystrokes to the backend. We intentionally do **not**
    /// parse the byte stream itself for command text — that would capture
    /// every typed character including passwords typed at a `sudo -i` /
    /// `ssh-keygen` prompt (see issue #69: the root password was leaking
    /// into the sidebar). Instead, we only watch for the Enter byte
    /// (`\r` 0x0d / `\n` 0x0a) as a *trigger*, and at that instant read the
    /// already-rendered prompt line back from the alacritty grid via
    /// [`Self::snapshot_command_from_grid`]. Because passwords are never
    /// echoed to the screen, they never enter the grid, and therefore
    /// never enter the command history — but real commands (`$ ls -l`)
    /// which the shell *has* echoed as the user typed them, do.
    pub fn write(&self, data: &[u8]) {
        // Fire the snapshot when the user presses Enter. We check the raw
        // bytes here (before forwarding) so the grid still reflects the
        // pre-Enter frame — the shell hasn't yet echoed the newline, so
        // the cursor sits at the end of the just-typed command line.
        // `snapshot_command_from_grid` is a no-op for empty / prompt /
        // password lines, so spurious fires (e.g. Shift+Enter, control
        // sequences that include a stray `\n`) are harmless.
        let has_enter = data.iter().any(|&b| b == 0x0d || b == 0x0a);
        if has_enter {
            self.snapshot_command_from_grid();
            // Re-sync from the shell's own history file as the ground-truth
            // source. `snapshot_command_from_grid` is a best-effort guess
            // (prompt stripping is heuristic — see `strip_prompt_prefix`),
            // but the shell's own `~/.bash_history` / `~/.zsh_history`
            // records exactly the commands executed and never the prompt
            // text, so overwriting the in-memory deque with the file's
            // contents on the next `HistoryLoaded` event corrects any
            // sporadic misread. Throttled to avoid `cat`ing the file on
            // every keystroke (and to avoid a remote `cat` over SSH on
            // every Enter). For shells that don't flush in real time
            // (default bash) the snapshot above still gives instant
            // feedback.
            const RESYNC_THROTTLE: std::time::Duration = std::time::Duration::from_millis(400);
            let should_resync = {
                let mut last = self.last_history_refresh.lock();
                let now = std::time::Instant::now();
                if now.duration_since(*last) >= RESYNC_THROTTLE {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if should_resync {
                self.refresh_history();
            }
        }
        self.backend.write(data);
    }

    /// Whether the kitty keyboard protocol is currently active for this
    /// session. When true, the UI encodes key events as `CSI u` and emits
    /// focus in/out events.
    pub fn kitty_keyboard_enabled(&self) -> bool {
        self.kitty_keyboard.load(Ordering::SeqCst) != 0
    }

    /// Scan a chunk of *program output* for kitty-keyboard negotiation
    /// requests and react accordingly.
    ///
    /// The kitty keyboard protocol is opt-in: a program must explicitly ask
    /// the terminal to switch key reporting to `CSI u`. We only flip
    /// [`Self::kitty_keyboard`] on after the program sends a request, so plain
    /// shells keep receiving ordinary key bytes.
    ///
    /// IMPORTANT: we intentionally do **NOT** write any response bytes back to
    /// the PTY. Writing a kitty reply (`\e[?u` / `\e[>1u`) would be forwarded by
    /// a multiplexer such as tmux to the pane's program, which (e.g. a plain
    /// shell) would echo it as literal garbage like `?u` / `>1u`. Instead we
    /// only track the local state: when the program requests `CSI u` we enable
    /// our own `CSI u` *output* encoding so the program still receives properly
    /// encoded keys, without injecting any bytes into the stream.
    ///
    /// Recognized requests (program → terminal):
    /// - `\e[?u`  capability query   → enable our CSI u output
    /// - `\e[>u`  enable request      → enable our CSI u output
    /// - `\e[=u`  enable request (alias) → enable our CSI u output
    /// - `\e[<u`  disable request     → disable
    ///
    /// NOTE: this scan is stateless per chunk, so a request split across a
    /// chunk boundary (e.g. `\e[?u` arriving as `\e[?` + `u`) is missed.
    /// Programs typically retry their capability query after a timeout, so
    /// this only delays enablement in practice. Do not "fix" it by scanning
    /// to an arbitrary `u` byte — that would false-positive on normal text.
    fn scan_kitty_negotiation(data: &[u8], state: &Arc<AtomicU8>) {
        // Only recognise the exact Kitty keyboard protocol CSI sequences:
        //   ESC [ ? u        query
        //   ESC [ > u        enable (progressive enhancement)
        //   ESC [ > Ps u     enable with flags
        //   ESC [ = u        enable
        //   ESC [ = Ps u     enable with flags
        //   ESC [ < u        disable
        // We deliberately do NOT scan to the next arbitrary 'u' byte: CSI
        // colour sequences (e.g. \e[01;32m) and ordinary text containing the
        // letter 'u' occur all the time, and matching them would flip kitty
        // keyboard state on and off spuriously.
        let mut i = 0;
        while i + 3 < data.len() {
            if data[i] == 0x1b && data[i + 1] == b'[' {
                let mut k = i + 2;
                // Optional leading parameter digits.
                while k < data.len() && data[k].is_ascii_digit() {
                    k += 1;
                }
                if k < data.len() && matches!(data[k], b'?' | b'>' | b'=' | b'<') {
                    let intermediate = data[k];
                    k += 1;
                    // `param_start` must point at the first parameter digit
                    // (or the sequence's final letter when there are none).
                    // Pointing it at the intermediate byte (`?`/`>`/`=`/`<`)
                    // made the alt-screen-exit check below parse e.g.
                    // `?1049` — which fails to parse as a number and
                    // silently disabled that whole branch.
                    let param_start = k;
                    // Optional trailing parameter digits.
                    while k < data.len() && data[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k < data.len() && data[k] == b'u' {
                        match intermediate {
                            b'?' | b'>' | b'=' => {
                                state.store(1, Ordering::SeqCst);
                                tracing::debug!(
                                    "kitty keyboard: enabled by program request {:?}",
                                    std::str::from_utf8(&data[i..=k])
                                );
                            }
                            b'<' => {
                                state.store(0, Ordering::SeqCst);
                                tracing::debug!(
                                    "kitty keyboard: disabled by program request {:?}",
                                    std::str::from_utf8(&data[i..=k])
                                );
                            }
                            _ => {}
                        }
                        i = k + 1;
                        continue;
                    }
                    // Not a kitty `u` sequence. If it is an alternate-screen
                    // *exit* (`?1049l`, `?1047l`, `?47l`), the TUI program is
                    // leaving its full-screen UI. Many programs (e.g. opencode)
                    // forget to send `ESC [ < u` to disable the kitty keyboard
                    // protocol, so we treat alt-screen exit as an implicit
                    // disable to avoid sending `CSI u` keystrokes to the plain
                    // shell that follows (which would render as garbage).
                    // Bounds-checked: `k` can equal `data.len()` when the
                    // chunk ends right after the parameter digits (e.g. a
                    // split `\x1b[?1049l`), and indexing past the end would
                    // panic the parsing task.
                    if k < data.len() && data[k] == b'l' {
                        let param = std::str::from_utf8(&data[param_start..k])
                            .unwrap_or("")
                            .parse::<u32>()
                            .unwrap_or(0);
                        if matches!(param, 1049 | 1047 | 47) {
                            if state.load(Ordering::SeqCst) != 0 {
                                state.store(0, Ordering::SeqCst);
                                tracing::debug!(
                                    "kitty keyboard: disabled on alt-screen exit {:?}",
                                    std::str::from_utf8(&data[i..=k])
                                );
                            }
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// Report a focus change to the program (kitty keyboard protocol).
    /// `focused == true` → `\e[I`, `false` → `\e[O`. No-op when kitty keyboard
    /// is disabled, since legacy terminals do not understand these sequences.
    pub fn report_focus(&self, focused: bool) {
        if !self.kitty_keyboard_enabled() {
            return;
        }
        let seq = if focused { b"\x1b[I" } else { b"\x1b[O" };
        self.backend.write(seq);
    }

    /// Write raw bytes to the backend. Functionally equivalent to
    /// [`Self::write`] now that user input is no longer captured; the
    /// distinct name is kept at call sites to flag programmatic paste
    /// (e.g. the History panel's "paste into input line" action) as
    /// opposed to user typing.
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
    /// (see [`Self::write`] / [`Self::snapshot_command_from_grid`]). The
    /// UI layer uses this to persist commands to the Store —
    /// `TerminalSession` itself has no storage dependency. Pass `None` to
    /// clear a previously-set callback.
    pub fn set_on_command(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        *self.on_command.lock() = cb;
    }

    /// Snapshot the cursor row of the alacritty grid as a command entry.
    ///
    /// Called by [`Self::write`] on detecting an Enter byte. The grid at
    /// that instant still shows the line the user just composed (e.g.
    /// `$ ls -l` with the cursor at the end), because the shell has not
    /// yet echoed back the newline. We read that row, strip a bunch of
    /// well-known prompt prefixes, drop lines that smell like password /
    /// passphrase prompts (which never contain the actual password — the
    /// password is absent from the grid entirely), and push the result to
    /// [`Self::command_history`].
    ///
    /// Safety property (the whole reason this replaces the old byte-stream
    /// capture): if the user is at a `sudo -i` password prompt and hits
    /// Enter, the grid row contains something like
    /// `[sudo] password for user:` — *no password characters*. We filter
    /// it out, and even if the filter missed it, the string that would
    /// land in history is the *prompt* text, never the secret itself.
    ///
    /// Multi-line commands, multiline REPL continuations, commands pasted
    /// as a single Enter-delimited blob, and commands edited mid-line with
    /// arrow keys are not captured perfectly — but the cross-check against
    /// the most recent entry (skipping an exact duplicate) and the LRU cap
    /// keep the history usable, and `refresh_history` re-syncs from the
    /// shell's own `~/.bash_history` / `~/.zsh_history` for ground truth.
    fn snapshot_command_from_grid(&self) {
        // Read the cursor row out of the term grid. Holding the term lock is
        // cheap; this runs only on Enter (no per-byte hot path).
        let raw = self.with_term(|term| {
            let grid = term.grid();
            let row = grid.cursor.point.line;
            let num_cols = grid.columns();
            let li = alacritty_terminal::index::Line(row.0);
            let mut s = String::with_capacity(num_cols);
            for col in 0..num_cols {
                let cell = &grid[li][alacritty_terminal::index::Column(col)];
                // Skip wide-char trailing spacers so a CJK command doesn't
                // carry phantom cells into history.
                if cell
                    .flags
                    .intersects(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                {
                    continue;
                }
                s.push(cell.c);
            }
            s
        });

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }

        // Drop lines that smell like password / passphrase / 2FA prompts.
        // These rows never contain the secret itself (passwords aren't
        // echoed), so wrongly keeping one would pollute history with the
        // *prompt*, not the password — but we'd rather avoid the pollution.
        let lower = trimmed.to_lowercase();
        if lower.contains("password")
            || lower.contains("passphrase")
            || lower.contains("[sudo]")
            || lower.contains("verification code")
        {
            return;
        }
        // A trailing `:` (optionally followed by a space) is the canonical
        // form of a password / passphrase / confirmation prompt — drop it.
        let trimmed_no_trailing_ws = trimmed.trim_end();
        if trimmed_no_trailing_ws.ends_with(':') {
            return;
        }

        let stripped = strip_prompt_prefix(trimmed);
        if stripped.is_empty() {
            return;
        }

        // LRU dedup + cap, mirroring the old `capture_command` behaviour so
        // the in-memory deque stays consistent with the Store-side dedup.
        let cmd = stripped.to_string();
        let cb = {
            let mut history = self.command_history.lock();
            if let Some(first) = history.front() {
                if first == &cmd {
                    // Exact-duplicate of the most-recent entry: skip. We
                    // don't promote (move to front) because it's already
                    // there.
                    return;
                }
            }
            if let Some(pos) = history.iter().position(|c| c == &cmd) {
                history.remove(pos);
            }
            if history.len() >= MAX_COMMAND_HISTORY {
                history.pop_back();
            }
            history.push_front(cmd.clone());
            self.on_command.lock().clone()
        };
        if let Some(cb) = cb {
            cb(&cmd);
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        // Guard against nonsensical / tiny sizes. A 0 or very small size would
        // make the underlying PTY (and a multiplexer such as tmux) shrink the
        // usable area to almost nothing, which looks like the terminal
        // "collapsing". This can happen if a resize is computed from a not-yet
        // laid-out view or an overlay bounds. Ignore those.
        if cols < 2 || rows < 1 {
            tracing::warn!("ignoring invalid resize request: {}x{}", cols, rows);
            return;
        }
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

    /// Handle a scroll-wheel event with the correct semantics for the
    /// terminal's current mode, mirroring alacritty's own input handling.
    ///
    /// - If the terminal is in a mouse-reporting mode (`MOUSE_MODE`) and
    ///   the user isn't holding Shift, send a mouse-button-4/5 (wheel
    ///   up/down) SGR or normal mouse report at the given cell coordinate.
    ///   This is what `vim`'s `:set mouse=a` and `less` rely on to scroll
    ///   in-place.
    /// - Else if the alternate screen is active *and* `ALTERNATE_SCROLL`
    ///   is enabled (the default when a program enters the alt screen via
    ///   `\e[?1049h` *and* requests `\e[?1007h`), translate the wheel into
    ///   application cursor-key sequences (`\eOA` / `\eOB`) so programs
    ///   like `vim` without mouse support can scroll as if the user were
    ///   pressing Up/Down. Shift bypasses this (and the mouse-report
    ///   branch above) so the user can still reach the scrollback when a
    ///   full-screen app is capturing the wheel.
    /// - Otherwise, scroll the primary screen's scrollback buffer by `lines`.
    ///
    /// `lines` is positive for scroll-down (wheel towards user) and
    /// negative for scroll-up. `cell` is the 0-based (column, row) of the
    /// pointer inside the terminal grid; pass `(0, 0)` when the caller
    /// doesn't track the pointer (the mouse-report payload will then
    /// report the top-left cell, which still works for programs that
    /// only care about the button code).
    /// `shift` should be true when the Shift modifier is held — it forces
    /// the scrollback branch even in alt-screen / mouse mode.
    pub fn handle_wheel(&self, lines: i32, cell: (usize, usize), shift: bool) {
        use alacritty_terminal::term::TermMode;

        let mode = *self.term.lock().mode();

        // (1) Mouse reporting mode — emit a wheel-button report.
        if mode.intersects(TermMode::MOUSE_MODE) && !shift {
            // Wheel codes: 64 = up, 65 = down. In GPUI `lines > 0` means
            // scroll-up (towards history) — matches alacritty's
            // `new_scroll_y_px > 0 == up` convention.
            let code: u8 = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs() as usize;
            let (col, row) = cell;
            let mut buf = Vec::with_capacity(count * 16);
            for _ in 0..count {
                if mode.contains(TermMode::SGR_MOUSE) {
                    // SGR mouse: \e[<button;col;rowM (press) / m (release).
                    // Wheel buttons fire as press + release in one shot,
                    // so we emit the press form ("M") — that's what
                    // xterm and vim's mouse handling expect for wheel.
                    buf.extend_from_slice(
                        format!("\x1b[<{};{};{}M", code, col + 1, row + 1).as_bytes(),
                    );
                } else {
                    // Legacy CSI M format: \e[M <32+button> <32+1+col> <32+1+row>.
                    // Clamp to the legacy 223-cell ceiling; cells beyond
                    // that are simply dropped (matches xterm).
                    if col < 222 && row < 222 {
                        buf.push(0x1b);
                        buf.push(b'[');
                        buf.push(b'M');
                        buf.push(32 + code);
                        buf.push(32 + 1 + col as u8);
                        buf.push(32 + 1 + row as u8);
                    }
                }
            }
            if !buf.is_empty() {
                self.backend.write(&buf);
            }
            return;
        }

        // (2) Alternate screen + ALTERNATE_SCROLL → cursor keys.
        // Shift bypasses so the user can reach scrollback in full-screen apps.
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !shift {
            let cmd: u8 = if lines > 0 { b'A' } else { b'B' };
            let count = lines.unsigned_abs() as usize;
            // Application cursor keys use `\eO A` (SS3) when APP_CURSOR
            // is set; otherwise the normal cursor mode `\e[A` is used.
            let prefix_a = if mode.contains(TermMode::APP_CURSOR) {
                b'O'
            } else {
                b'['
            };
            let mut buf = Vec::with_capacity(count * 3);
            for _ in 0..count {
                buf.push(0x1b);
                buf.push(prefix_a);
                buf.push(cmd);
            }
            self.backend.write(&buf);
            return;
        }

        // (3) Default: scroll the scrollback buffer.
        self.scroll(lines);
    }

    /// Report a mouse button press / release / drag to the program when mouse
    /// reporting is active, mirroring `handle_wheel`'s encoding.
    ///
    /// `button` is the xterm button number: `0` = left, `1` = middle,
    /// `2` = right. `pressed` is true for a press (or a drag move) and false
    /// for a release. `dragging` is true while a button is held and the
    /// pointer moves. `cell` is the 0-based `(col, row)` of the pointer inside
    /// the terminal grid.
    ///
    /// Returns `true` if a mouse report was emitted (caller should then skip
    /// its local selection logic), or `false` if the program isn't in mouse
    /// mode (so the caller may handle the event as a plain selection).
    pub fn report_mouse_button(
        &self,
        button: u8,
        cell: (usize, usize),
        pressed: bool,
        dragging: bool,
    ) -> bool {
        let mode = *self.term.lock().mode();

        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }

        // Build the xterm button code.
        //   base button: 0=left, 1=middle, 2=right
        //   +0x20: motion while a button is held (drag)
        // SGR (1006) distinguishes press/release via the final byte
        // ('M' for press, 'm' for release); legacy CSI M encodes
        // release as `button + 3` instead.
        let mut code = button;
        if dragging {
            code |= 0x20;
        }

        let (col, row) = cell;
        let mut buf = Vec::with_capacity(16);
        if mode.contains(TermMode::SGR_MOUSE) {
            // SGR mouse: \e[<code;col;rowM (press/drag) / m (release).
            let suffix = if pressed { b'M' } else { b'm' };
            buf.extend_from_slice(
                format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, suffix as char).as_bytes(),
            );
        } else {
            // Legacy CSI M format: \e[M <32+code> <32+1+col> <32+1+row>.
            if !pressed {
                code = button + 3;
            }
            if col < 222 && row < 222 {
                buf.push(0x1b);
                buf.push(b'[');
                buf.push(b'M');
                buf.push(32 + code);
                buf.push(32 + 1 + col as u8);
                buf.push(32 + 1 + row as u8);
            }
        }
        if !buf.is_empty() {
            self.backend.write(&buf);
            return true;
        }
        // Nothing was emitted (e.g. legacy format coordinate overflow) —
        // report `false` so the caller falls back to its local handling.
        false
    }

    // -----------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------

    /// Find all matches of `query` in the terminal grid (scrollback +
    /// visible area). Returns an empty vec if the query is empty or the
    /// regex fails to compile.
    ///
    /// The query is treated as a regex; callers that want literal matching
    /// should pre-escape it with `regex::escape`.
    pub fn search_matches(&self, query: &str) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut regex = match RegexSearch::new(query) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let term = self.term.lock();
        let start = AlacPoint::new(term.grid().topmost_line(), Column(0));
        let end = AlacPoint::new(term.grid().bottommost_line(), term.grid().last_column());
        RegexIter::new(start, end, AlacDirection::Right, &*term, &mut regex)
            .map(SearchMatch::from_alacritty)
            .collect()
    }

    /// Scroll the terminal viewport so that `m` is visible, and return the
    /// grid line of the match start (for selection highlighting).
    pub fn scroll_to_match(&self, m: &SearchMatch) {
        let mut term = self.term.lock();
        let point = AlacPoint::new(Line(m.start_line), Column(m.start_col));
        term.scroll_to_point(point);
        let _ = self.wakeup_tx.try_broadcast(());
    }

    /// Returns the cursor's grid line + column, used by the search UI to
    /// determine which match is "active" (closest to the cursor).
    pub fn cursor_point(&self) -> (i32, usize) {
        let term = self.term.lock();
        let p = term.grid().cursor.point;
        (p.line.0, p.column.0)
    }
}
