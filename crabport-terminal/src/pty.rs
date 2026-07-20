//! Local PTY backend.
//!
//! Drives a child shell process attached to a pseudoterminal, pumping PTY
//! output into the [`BackendEvent`] broadcast channel as `Data` chunks so
//! the `TerminalSession` can parse them into its own alacritty `Term`.
//!
//! On both platforms the high-level structure is the same:
//!
//! 1. Create the PTY + child shell process via `alacritty_terminal::tty::new`.
//! 2. A reader thread pumps PTY output into the [`BackendEvent`] broadcast
//!    channel (`Data` for every chunk, `Closed` on EOF / read error).
//! 3. A writer task drains a command channel into the PTY's writer handle.
//! 4. A child-exit watcher broadcasts `BackendEvent::Closed` once the child
//!    shell terminates.
//!
//! The Unix implementation uses `alacritty_terminal::tty::new` (which wraps
//! `openpty` + `fork`) and `libc::waitpid` to watch the child, since
//! `alacritty_terminal`'s Unix `Pty` exposes `file()` / `child()` accessors.
//!
//! The Windows implementation also uses `alacritty_terminal::tty::new`,
//! which on Windows calls `conpty::new` — this auto-detects Windows
//! Terminal's improved `conpty.dll`/`OpenConsole.exe` if available, and
//! falls back to the inbox Windows ConPTY API. We do NOT use
//! `alacritty_terminal`'s `EventLoop` even though it exists and Zed uses
//! it — that loop consumes bytes into its own `Term`, but our architecture
//! has `TerminalSession` own the `Term` and parse `BackendEvent::Data`
//! chunks itself. Driving the `Pty` directly (via its `EventedReadWrite`
//! `reader()`/`writer()` methods, wrapped in `Arc<Mutex<Pty>>`) keeps the
//! Windows path on the same byte-broadcast model as Unix.
//!
//! Shell selection on Windows cascades `pwsh.exe` → `powershell.exe` →
//! `cmd.exe` (see `default_shell`). On Unix, `alacritty_terminal` resolves
//! the login shell from `passwd`/`$SHELL`.

// Imports used only by the Unix implementation.
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    thread,
    time::Duration,
};

// Shared across platforms — struct fields and the CrabPortMonitor impl use these.
use std::{
    sync::Arc,
    sync::OnceLock,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    time::SystemTime,
};

#[cfg(windows)]
use alacritty_terminal::tty::Pty;
#[cfg(unix)]
use alacritty_terminal::{
    event::WindowSize,
    tty::{self, Options, Pty},
};
#[cfg(unix)]
use async_broadcast::broadcast;
#[cfg(unix)]
use libc::{TIOCSWINSZ, ioctl, winsize};
// `BroadcastReceiver` is the return type of `CrabPortTerminal::subscribe`,
// which is implemented for all platforms.
use async_broadcast::Receiver as BroadcastReceiver;
use async_channel::{Sender as MpscSender, unbounded};
use parking_lot::{Mutex, RwLock};

use crate::terminal::{
    BackendEvent, CpuStats, CrabPortMonitor, CrabPortTerminal, DiskStats, MemoryStats,
    NetworkStats, RemoteMetrics, RemoteStatus,
};

// ===========================================================================
// Platform-agnostic shell selection
// ===========================================================================

/// Ensure the PTY child shell runs in a UTF-8 capable locale.
///
/// See [`PtyBackend::new`] for why this is needed: GUI-launched processes
/// on macOS don't inherit a usable `LANG` — the process starts with
/// `LANG=""` and macOS's quirky `LC_CTYPE="UTF-8"` (which is not a valid
/// POSIX locale name). The shell itself is lenient about this so input
/// parsing works, but any child program that calls `setlocale(LC_ALL, "")`
/// (`ls`, `echo`, …) rejects the bogus `LC_CTYPE`, silently falls back to
/// the `C` locale, and emits `????` for any non-ASCII byte.
///
/// Fix: if `LANG` is missing or `C`/`POSIX`, force it to `en_US.UTF-8`
/// (shipped by default on every macOS install, and a UTF-8 codeset is all
/// that's needed to correctly round-trip CJK / emoji bytes — the language
/// part of the locale doesn't restrict which characters can be displayed).
/// Also set `LC_CTYPE` to a real locale name so it stops being the bogus
/// `"UTF-8"` value. We don't try to derive a locale matching the user's
/// UI language because macOS doesn't ship script-tagged locales (e.g.
/// `zh_Hans_CN.UTF-8` doesn't exist — only `zh_CN.UTF-8`), so naive
/// conversion from BCP 47 produces names `setlocale` rejects.
///
/// If the user has explicitly set `LANG` to something real (e.g.
/// `zh_CN.UTF-8`), we respect it.
fn ensure_utf8_locale() {
    // A locale is "good" if it's non-empty and not the `C` / `POSIX`
    // default (which is ASCII-only). We don't otherwise validate the name —
    // if the user set `LANG=zh_CN.UTF-8` we trust them.
    let lang_ok = std::env::var("LANG")
        .ok()
        .filter(|s| !s.is_empty() && s != "C" && s != "POSIX")
        .is_some();

    if lang_ok {
        return;
    }

    // Default to `en_US.UTF-8` — always installed on macOS, and a UTF-8
    // codeset is sufficient for correct CJK / emoji handling regardless of
    // the language part.
    // SAFETY: setting env vars is process-global but our PTY is the only
    // child we spawn, and we do so immediately after this in
    // [`PtyBackend::new`]. No other thread is reading env vars concurrently
    // at this point.
    unsafe {
        std::env::set_var("LANG", "en_US.UTF-8");
        // Overwrite the bogus `"UTF-8"` value macOS injects — a real locale
        // name is required for `setlocale` in child programs.
        std::env::set_var("LC_CTYPE", "en_US.UTF-8");
    }
}

/// Pick the default local shell for this platform, mirroring alacritty's
/// own `tty::windows::cmdline` default (which is `powershell`) plus a
/// fallback cascade through modern PowerShell Core to the legacy
/// command prompt.
///
/// - **Windows**: prefer `pwsh.exe` (PowerShell 7+, the modern Core
///   build) when installed, then `powershell.exe` (the inbox Windows
///   PowerShell 5.x that's always present on Windows 10+), then
///   `cmd.exe` as the ultimate fallback. We do NOT shell out to
///   `wt.exe` (Windows Terminal) — that's a *launcher* that opens its
///   own GUI window, not a shell that can run inside our ConPTY.
///   Alacritty's own default is `powershell`, so we match that when
///   `pwsh.exe` isn't available.
/// - **Unix**: `None` — `alacritty_terminal` already resolves the user's
///   login shell from `passwd` / `$SHELL`, which is what we want.
fn default_shell() -> Option<(String, Vec<String>)> {
    #[cfg(target_os = "windows")]
    {
        // Prefer PowerShell 7+ (`pwsh.exe`) when the user has installed
        // it — it's a noticeable upgrade over the inbox Windows
        // PowerShell (faster, cross-platform, better Unicode handling).
        if which_executable("pwsh.exe").is_some() {
            return Some(("pwsh.exe".to_string(), vec![]));
        }
        // Fall back to inbox Windows PowerShell — always present on
        // Windows 10+. This matches alacritty's own `tty::windows::cmdline`
        // default (`Shell::new("powershell".to_owned(), Vec::new())`).
        if which_executable("powershell.exe").is_some() {
            return Some(("powershell.exe".to_string(), vec![]));
        }
        // Last resort: the legacy Windows command prompt. Always present.
        Some(("cmd.exe".to_string(), vec![]))
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Let alacritty_terminal pick the login shell from passwd/$SHELL.
        None
    }
}

/// Return `Some(())` if `program` resolves on `PATH` (or is an absolute
/// path that exists), `None` otherwise. On Windows we also try the
/// PATHEXT extensions (`.EXE`, `.CMD`, …) when looking up a bare name.
#[cfg(windows)]
fn which_executable(program: &str) -> Option<()> {
    // Absolute / relative path with a separator → check the file directly.
    if program.contains('\\') || program.contains('/') {
        return std::path::Path::new(program).is_file().then_some(());
    }
    // Bare program name → PATHEXT-aware PATH walk.
    let path = std::env::var("PATH").ok()?;
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE".to_string());
    for ext in pathext.split(';') {
        for dir in path.split(';') {
            let candidate = std::path::Path::new(dir).join(format!("{}{}", program, ext));
            if candidate.is_file() {
                return Some(());
            }
        }
    }
    None
}

// ===========================================================================
// Global local-metrics cache — shared across all PtyBackend instances
// ===========================================================================
//
// Every local PTY pane (whether a separate tab or a split pane) collects
// the *same* host metrics — CPU, memory, disk, network are all properties
// of the machine, not of the individual shell process. Without this cache
// each `PtyBackend::metrics()` call would run its own `sysinfo::System::
// refresh_*` + `Disks::refresh` + `Networks::refresh`, which is wasteful
// (N panes → N sysinfo refreshes per second) and can cause visible
// jitter in the toolbar when the refreshes race each other.
//
// The cache is a process-global singleton: the first `PtyBackend` to call
// `metrics()` initializes it, and every subsequent `PtyBackend` reads from
// the same snapshot. The snapshot is refreshed at most once per second —
// the same cadence the old per-backend code used — so the toolbar still
// updates live, just without the duplicate work.
//
// SSH backends are unaffected: they run their own `monitor_loop` over SSH
// exec, and split panes on an SSH tab already share the parent's monitor
// state (see `SshBackend::new_channel_backend`).
struct LocalMetricsCache {
    /// Cached snapshot. Replaced atomically (via the lock) every refresh tick.
    snapshot: RwLock<RemoteMetrics>,
    /// Monotonic millis of the last refresh. Accessed via a mutex alongside
    /// the refresh itself so only one thread refreshes at a time.
    last_refresh_ms: AtomicU64,
    /// Guard so only one thread performs the refresh; other threads just
    /// read the existing snapshot. Implemented as a Mutex<bool> flag —
    /// `try_lock` succeeds when no refresh is in progress.
    refreshing: Mutex<bool>,
    // sysinfo state is kept inside the cache so it retains its internal
    // counters between refreshes (sysinfo computes CPU usage as a diff,
    // so the first refresh after a fresh `System::new()` always returns 0).
    sys: RwLock<sysinfo::System>,
    networks: RwLock<sysinfo::Networks>,
    disks: RwLock<sysinfo::Disks>,
    /// Previous cumulative network bytes (for computing per-second rate).
    prev_net_sent: AtomicU64,
    prev_net_recv: AtomicU64,
}

impl LocalMetricsCache {
    fn new() -> Self {
        Self {
            snapshot: RwLock::new(RemoteMetrics::default()),
            last_refresh_ms: AtomicU64::new(0),
            refreshing: Mutex::new(false),
            sys: RwLock::new(sysinfo::System::new()),
            networks: RwLock::new(sysinfo::Networks::new_with_refreshed_list()),
            disks: RwLock::new(sysinfo::Disks::new_with_refreshed_list()),
            prev_net_sent: AtomicU64::new(0),
            prev_net_recv: AtomicU64::new(0),
        }
    }

    /// Return the current metrics snapshot, refreshing it first if it's
    /// older than 1 second. Only one thread performs the refresh; others
    /// fall through to read the existing snapshot. This keeps the toolbar
    /// responsive even when N panes all call `metrics()` in the same frame.
    fn metrics(&self) -> RemoteMetrics {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last = self.last_refresh_ms.load(AtomicOrdering::Relaxed);
        if now.saturating_sub(last) >= 1000 {
            // Try to claim the refresh slot. If another thread is already
            // refreshing, skip — we'll just read the slightly-stale snapshot.
            if let Some(mut guard) = self.refreshing.try_lock() {
                if !*guard {
                    *guard = true;
                    drop(guard);
                    self.refresh(now);
                    *self.refreshing.lock() = false;
                }
            }
        }
        self.snapshot.read().clone()
    }

    fn refresh(&self, now: u64) {
        {
            let mut sys = self.sys.write();
            sys.refresh_memory();
            sys.refresh_cpu_usage();
        }
        {
            let mut networks = self.networks.write();
            networks.refresh(true);
        }
        {
            let mut disks = self.disks.write();
            disks.refresh(true);
        }

        let sys = self.sys.read();
        let memory = MemoryStats {
            total: sys.total_memory(),
            used: sys.used_memory(),
        };
        let cpu = CpuStats {
            usage_pct: sys.global_cpu_usage(),
        };
        drop(sys);

        let networks = self.networks.read();
        let mut bytes_sent: u64 = 0;
        let mut bytes_recv: u64 = 0;
        for (_name, network) in networks.iter() {
            bytes_sent += network.transmitted();
            bytes_recv += network.received();
        }
        let prev_sent = self.prev_net_sent.swap(bytes_sent, AtomicOrdering::Relaxed);
        let prev_recv = self.prev_net_recv.swap(bytes_recv, AtomicOrdering::Relaxed);
        let network = NetworkStats {
            bytes_sent: bytes_sent.saturating_sub(prev_sent),
            bytes_recv: bytes_recv.saturating_sub(prev_recv),
        };

        let disk = {
            let disks_guard = self.disks.read();
            pick_primary_disk(disks_guard.list())
        };

        let mut cached = self.snapshot.write();
        *cached = RemoteMetrics {
            latency_ms: None,
            memory: Some(memory),
            network: Some(network),
            cpu: Some(cpu),
            disk,
        };
        self.last_refresh_ms.store(now, AtomicOrdering::Relaxed);
    }
}

/// Process-global local-metrics cache. Initialized on first use.
fn local_metrics_cache() -> &'static LocalMetricsCache {
    static CACHE: OnceLock<LocalMetricsCache> = OnceLock::new();
    CACHE.get_or_init(LocalMetricsCache::new)
}

// ===========================================================================
// Command enum — shared between platforms
// ===========================================================================

enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

// ===========================================================================
// Disk selection helper — pick the primary disk to surface in metrics
// ===========================================================================

/// Pick the disk whose usage we surface in [`RemoteMetrics::disk`]. The
/// rule is intentionally simple — users typically only care about their
/// "main" disk, and enumerating every mount would make the toolbar chip
/// unreadable. Selection order:
///
/// 1. On Unix, the mount whose `mount_point` is the prefix of the user's
///    home directory. This is the disk that actually matters for "is my
///    home directory full". On Windows, we instead match the system drive
///    (the root of `%SystemRoot%`, usually `C:\`).
/// 2. Otherwise the disk with the largest `total_space` — a reasonable
///    proxy for "primary disk" on hosts where (1) didn't match.
/// 3. Returns `None` when `disks` is empty (e.g. on a chroot / container
///    where sysinfo can't enumerate mounts).
fn pick_primary_disk(disks: &[sysinfo::Disk]) -> Option<DiskStats> {
    if disks.is_empty() {
        return None;
    }

    // (1) Match by home / system drive.
    let pick = disks.iter().find(|d| {
        let mp = d.mount_point();
        #[cfg(unix)]
        {
            if let Some(home) = std::env::var_os("HOME") {
                let home = std::path::Path::new(&home);
                return mp == home || home.starts_with(mp);
            }
            false
        }
        #[cfg(windows)]
        {
            // On Windows the system drive is the root of %SystemRoot%
            // (e.g. `C:\\`). sysinfo returns mount points as `\\\\?\\C:\\`
            // or `C:\\`, so we compare the first non-prefix character.
            if let Ok(sysroot) = std::env::var("SystemRoot") {
                let sysroot = std::path::Path::new(&sysroot);
                if let Some(sysroot_root) = sysroot.ancestors().nth(1) {
                    return mp == sysroot_root;
                }
            }
            false
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    });

    // (2) Fallback to the largest disk.
    let pick = pick.or_else(|| {
        disks
            .iter()
            .max_by_key(|d| d.total_space())
            .filter(|d| d.total_space() > 0)
    });

    let d = pick?;
    let total = d.total_space();
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(d.available_space());
    Some(DiskStats { total, used })
}

// ===========================================================================
// PtyBackend — common fields
// ===========================================================================

pub struct PtyBackend {
    command_tx: MpscSender<Command>,
    event_tx: async_broadcast::Sender<BackendEvent>,
    // `Arc<Mutex<Pty>>` — kept alive for the lifetime of the backend so
    // the reader/writer/resize worker threads (which hold clones) can
    // reach the underlying PTY. On Unix this is `alacritty_terminal::tty::Pty`
    // (wrapping `openpty`+`fork`); on Windows it's the same type wrapping
    // ConPTY. Both expose `file()`/`child()` (Unix) or `reader()`/`writer()`/`on_resize()`
    // (Windows) through the traits `tty::EventedReadWrite` / `tty::EventedPty`.
    _pty: Arc<Mutex<Pty>>,
    /// Inactive receiver that keeps the broadcast channel alive even when
    /// there are no active subscribers. Without holding one receiver the
    /// channel reports `Closed` to new subscribers immediately.
    _event_rx: async_broadcast::InactiveReceiver<BackendEvent>,
}

// ===========================================================================
// Unix implementation
// ===========================================================================

#[cfg(unix)]
impl PtyBackend {
    pub fn new(cols: u16, rows: u16) -> std::io::Result<Self> {
        tty::setup_env();
        // alacritty's `setup_env` only sets `TERM` / `COLORTERM`. A shell
        // spawned from a GUI app (e.g. launched from Finder / Spotlight on
        // macOS) starts with `LANG=""` and macOS's bogus `LC_CTYPE="UTF-8"`
        // (not a valid POSIX locale name). The shell itself is lenient so
        // input parsing works, but any child program that calls
        // `setlocale(LC_ALL, "")` (`ls`, `echo`, …) rejects the locale and
        // falls back to `C`, emitting `????` for non-ASCII bytes.
        //
        // Force a real UTF-8 locale (`en_US.UTF-8`, shipped by default on
        // every macOS install) when the user hasn't set one. A UTF-8 codeset
        // is all that's needed to round-trip CJK / emoji — the language part
        // doesn't restrict which characters display. We respect any `LANG`
        // the user explicitly set.
        ensure_utf8_locale();

        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        };

        // Build alacritty `Options` with our default shell selection.
        // `default_shell()` returns `None` on Unix so alacritty picks the
        // login shell itself; on Windows it returns a `(program, args)`
        // tuple selecting `pwsh` → `powershell` → `cmd`.
        let mut options = Options::default();
        if let Some((program, args)) = default_shell() {
            options.shell = Some(tty::Shell::new(program, args));
        }

        let pty = Arc::new(Mutex::new(tty::new(&options, window_size, 0)?));

        let reader = pty.lock().file().try_clone()?;
        let mut writer = pty.lock().file().try_clone()?;

        let (event_tx, event_rx) = broadcast(1024);
        let _event_rx = event_rx.deactivate();

        let (command_tx, command_rx) = unbounded::<Command>();

        {
            let event_tx = event_tx.clone();

            thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 8192];

                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            tracing::info!("pty reader: EOF");
                            let _ = smol::block_on(event_tx.broadcast(BackendEvent::Closed));
                            break;
                        }

                        Ok(n) => {
                            tracing::debug!("pty reader: {} bytes", n);
                            let _ = smol::block_on(
                                event_tx.broadcast(BackendEvent::Data(buf[..n].to_vec())),
                            );
                        }

                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            // Non-blocking fd has no data yet — back off and retry.
                            thread::sleep(Duration::from_millis(10));
                        }

                        Err(err) => {
                            tracing::error!("pty reader error: {}", err);
                            let _ = smol::block_on(
                                event_tx.broadcast(BackendEvent::Error(err.to_string())),
                            );
                            break;
                        }
                    }
                }
            });
        }

        {
            let pty = pty.clone();
            let event_tx = event_tx.clone();

            smol::spawn(async move {
                while let Ok(cmd) = command_rx.recv().await {
                    match cmd {
                        Command::Write(data) => {
                            let _ = writer.write_all(&data);
                            let _ = writer.flush();
                        }

                        Command::Resize(cols, rows) => {
                            let fd = pty.lock().file().as_raw_fd();

                            let ws = winsize {
                                ws_row: rows,
                                ws_col: cols,
                                ws_xpixel: 0,
                                ws_ypixel: 0,
                            };

                            unsafe {
                                ioctl(fd, TIOCSWINSZ, &ws);
                            }
                        }

                        Command::Close => {
                            // Actively terminate the child shell so its
                            // descendants (vim, top, …) get SIGHUP via
                            // the kernel's session-leader semantics.
                            // Just closing our writer fd isn't enough —
                            // the reader thread still holds a clone of the
                            // master fd, so the PTY never reaches EOF and
                            // the child never sees HUP. Sending SIGTERM
                            // (or SIGHUP) to the child's pid makes the
                            // shell exit, which in turn tears down its
                            // process group.
                            #[cfg(unix)]
                            {
                                let pid = pty.lock().child().id();
                                if pid > 0 {
                                    // SIGHUP is the canonical "terminal
                                    // hung up" signal; shells propagate
                                    // it to their jobs.
                                    unsafe {
                                        libc::kill(pid as libc::pid_t, libc::SIGHUP);
                                    }
                                }
                            }
                            #[cfg(windows)]
                            {
                                // ConPTY doesn't expose the child pid via
                                // `child()`; the pseudoconsole is torn down
                                // when the `Pty` drops, which sends the
                                // equivalent of a console close event.
                            }
                            let _ = event_tx.broadcast(BackendEvent::Closed).await;
                            break;
                        }
                    }
                }
            })
            .detach();
        }

        {
            let event_tx = event_tx.clone();
            let child_pid = pty.lock().child().id();

            thread::spawn(move || {
                unsafe {
                    let mut status: libc::c_int = 0;
                    libc::waitpid(child_pid as i32, &mut status, 0);
                }

                let _ = smol::block_on(event_tx.broadcast(BackendEvent::Closed));
            });
        }

        Ok(Self {
            _pty: pty,
            command_tx,
            event_tx,
            _event_rx,
        })
    }
}

// ===========================================================================
// Windows implementation — alacritty's ConPTY `Pty` + manual reader/writer
// ===========================================================================
//
// We use `alacritty_terminal::tty::new` to create the ConPTY + child shell,
// which gives us automatic `conpty.dll`/`OpenConsole.exe` detection (the
// Windows Terminal project's improved ConPTY host) for free — matching
// alacritty's and Zed's behavior. The shell selection (`pwsh.exe` →
// `powershell.exe` → `cmd.exe`) is handled by our `default_shell()` above.
//
// We do NOT use alacritty's `EventLoop` here, even though it exists and
// Zed uses it. The reason is architectural: our `TerminalSession` owns
// its own `Term` and parses `BackendEvent::Data` byte chunks into it
// (see `TerminalSession::start`). alacritty's `EventLoop`, by contrast,
// owns the `Pty` *and* advances bytes into a `Term` it also owns — so
// the session would never see the raw bytes and its grid would stay
// empty. Zed solves this by sharing the `Term` between the event loop
// and the renderer; we don't want to refactor `TerminalSession` to that
// model just for Windows, so instead we drive the `Pty` directly with
// the same reader-thread / writer-task / child-watcher layout the Unix
// path uses.
//
// The `Pty` on Windows doesn't expose `file()` / `child()` (the ConPTY
// reader/writer are IOCP-backed pipes, not file descriptors), so we
// access them through the `EventedReadWrite` trait's `reader()` /
// `writer()` methods. Because those take `&mut self`, we wrap the `Pty`
// in `Arc<Mutex<Pty>>` and lock it briefly for each I/O operation —
// the same approach the Unix path takes for resize. The lock is never
// held across a blocking read/write syscall that could stall the other
// side, because alacritty's Windows `UnblockedReader`/`UnblockedWriter`
// use background threads + async pipes internally, so `read()`/`write()`
// return immediately with whatever is buffered.

#[cfg(windows)]
impl PtyBackend {
    pub fn new(cols: u16, rows: u16) -> std::io::Result<Self> {
        use alacritty_terminal::{
            event::WindowSize,
            tty::{self, Options, Pty},
        };

        // Build alacritty `Options` with our default shell selection.
        // `default_shell()` returns `Some((program, args))` on Windows,
        // cascading `pwsh.exe` → `powershell.exe` → `cmd.exe`.
        let mut options = Options::default();
        if let Some((program, args)) = default_shell() {
            options.shell = Some(tty::Shell::new(program, args));
        }
        options.drain_on_exit = true;

        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        };

        // `tty::new` on Windows calls `conpty::new`, which auto-detects
        // `conpty.dll` (Windows Terminal's improved ConPTY) and falls
        // back to the inbox Windows API. The returned `Pty` owns the
        // ConPTY handle + child process + reader/writer pipes.
        let pty: Arc<Mutex<Pty>> = Arc::new(Mutex::new(tty::new(&options, window_size, 0)?));

        // Broadcast channel for backend events.
        let (event_tx, event_rx) = async_broadcast::broadcast(1024);
        let _event_rx = event_rx.deactivate();

        // --- Reader thread: lock `pty`, call `reader().read()`, broadcast `Data` ---
        //
        // alacritty's Windows `UnblockedReader` runs a background thread
        // that drains the ConPTY conout pipe into an async `piper` pipe.
        // Our `read()` call here pulls from that pipe, so it returns
        // quickly (0 bytes if nothing is buffered yet). When we get a
        // 0-byte read we poll `next_child_event()` (set up by alacritty's
        // `ChildExitWatcher` via `RegisterWaitForSingleObject`) to detect
        // shell exit, then sleep briefly before retrying.
        //
        // The 5ms back-off is a pragmatic trade-off: a shorter interval
        // burns more CPU, a longer one adds latency. The proper fix would
        // be to use alacritty's `EventLoop` (which polls via IOCP), but
        // that consumes bytes into its own `Term`, breaking our
        // `BackendEvent::Data` broadcast model. 200Hz idle polling is
        // cheap and keeps the latency below human perception.
        {
            let event_tx = event_tx.clone();
            let pty_for_reader = pty.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    let n = {
                        let mut guard = pty_for_reader.lock();
                        use alacritty_terminal::tty::EventedReadWrite;
                        use std::io::Read;
                        match guard.reader().read(&mut buf) {
                            Ok(n) => n,
                            Err(e) => {
                                tracing::error!("conpty reader error: {}", e);
                                let _ = smol::block_on(
                                    event_tx.broadcast(BackendEvent::Error(e.to_string())),
                                );
                                let _ = smol::block_on(event_tx.broadcast(BackendEvent::Closed));
                                return;
                            }
                        }
                    };
                    if n == 0 {
                        // No data buffered right now. Check if the child
                        // has exited; if so, broadcast `Closed` and exit.
                        // Otherwise back off briefly.
                        let child_exited = {
                            let mut guard = pty_for_reader.lock();
                            use alacritty_terminal::tty::EventedPty;
                            guard.next_child_event().is_some()
                        };
                        if child_exited {
                            tracing::info!("conpty reader: child exited");
                            let _ = smol::block_on(event_tx.broadcast(BackendEvent::Closed));
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    tracing::debug!("conpty reader: {} bytes", n);
                    let _ =
                        smol::block_on(event_tx.broadcast(BackendEvent::Data(buf[..n].to_vec())));
                }
            });
        }

        // --- Command pump: writes + resizes + close → ConPTY ---
        let (command_tx, command_rx) = unbounded::<Command>();
        let pty_for_writer = pty.clone();
        let event_tx_for_writer = event_tx.clone();
        smol::spawn(async move {
            while let Ok(cmd) = command_rx.recv().await {
                match cmd {
                    Command::Write(data) => {
                        let mut guard = pty_for_writer.lock();
                        use alacritty_terminal::tty::EventedReadWrite;
                        use std::io::Write;
                        let _ = guard.writer().write_all(&data);
                        let _ = guard.writer().flush();
                    }
                    Command::Resize(cols, rows) => {
                        use alacritty_terminal::event::OnResize;
                        pty_for_writer.lock().on_resize(WindowSize {
                            num_lines: rows,
                            num_cols: cols,
                            cell_width: 0,
                            cell_height: 0,
                        });
                    }
                    Command::Close => {
                        let _ = event_tx_for_writer.try_broadcast(BackendEvent::Closed);
                        break;
                    }
                }
            }
        })
        .detach();

        Ok(Self {
            _pty: pty,
            command_tx,
            event_tx,
            _event_rx,
        })
    }
}

// ===========================================================================
// CrabPortTerminal impl — shared between platforms
// ===========================================================================

impl CrabPortTerminal for PtyBackend {
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

    fn spawn_channel(&self, cols: u16, rows: u16) -> Option<std::sync::Arc<dyn CrabPortTerminal>> {
        // Local PTY: just spawn a brand-new shell process. There's no
        // "connection" to share — each pane gets its own independent PTY.
        match PtyBackend::new(cols, rows) {
            Ok(backend) => Some(std::sync::Arc::new(backend)),
            Err(e) => {
                tracing::error!("PtyBackend spawn_channel failed: {e}");
                None
            }
        }
    }

    fn refresh_history(&self) {
        let event_tx = self.event_tx.clone();
        // Reading a couple of small files is cheap; do it on a background
        // thread so we never block the UI thread on disk I/O.
        std::thread::spawn(move || {
            let cmds = read_local_shell_history();
            let _ = event_tx.try_broadcast(BackendEvent::HistoryLoaded(cmds));
        });
    }
}

// ===========================================================================
// CrabPortMonitor impl — shared between platforms
// ===========================================================================

impl CrabPortMonitor for PtyBackend {
    fn status(&self) -> RemoteStatus {
        RemoteStatus::Local
    }

    fn metrics(&self) -> RemoteMetrics {
        // All local PTY panes share a single process-global metrics cache
        // — CPU / memory / disk / network are properties of the host, not
        // of the individual shell, so there's no point in each pane
        // running its own sysinfo refresh. The cache refreshes at most
        // once per second (same cadence as before) and is thread-safe.
        // See `LocalMetricsCache` for the rationale.
        local_metrics_cache().metrics()
    }
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        // Tell the writer task to shut down. `Command::Close` broadcasts
        // `BackendEvent::Closed` and exits the writer task's loop. On Unix
        // the reader thread then sees EOF on the cloned file descriptor
        // (closed when the child exits) and exits on its own. On Windows
        // the reader thread's `next_child_event()` poll picks up the exit
        // and the loop terminates. The `Pty` itself is dropped when the
        // last `Arc<Mutex<Pty>>` reference goes away — the writer task
        // holds one clone, which is dropped when its loop exits.
        let _ = self.command_tx.try_send(Command::Close);
    }
}

// ===========================================================================
// FailedPtyBackend — local-PTY fallback used when `PtyBackend::new` errors
// ===========================================================================

/// Degenerate backend used when local PTY creation fails (e.g. on a Windows
/// build without ConPTY, or on a headless host without a controlling TTY).
///
/// Instead of panicking — which would abort the whole app under the
/// `panic = abort` release profile — we surface the error through the
/// connection overlay so the user sees the message and can still open
/// remote SSH / Telnet sessions. The backend immediately reports `Closed`
/// on its first (and only) event, and writes / resizes are silently
/// dropped.
pub struct FailedPtyBackend {
    event_tx: async_broadcast::Sender<BackendEvent>,
    _event_rx: async_broadcast::InactiveReceiver<BackendEvent>,
}

impl FailedPtyBackend {
    pub fn new(message: String) -> Self {
        let (event_tx, event_rx) = async_broadcast::broadcast(16);
        let _event_rx = event_rx.deactivate();
        // Fire an `Error` (so the overlay logs it) followed by `Closed`
        // (so the session marks itself as done). Both are best-effort —
        // if no subscriber is attached yet the broadcast silently drops.
        let tx = event_tx.clone();
        std::thread::spawn(move || {
            let _ = smol::block_on(tx.broadcast(BackendEvent::Error(message)));
            let _ = smol::block_on(tx.broadcast(BackendEvent::Closed));
        });
        Self {
            event_tx,
            _event_rx,
        }
    }
}

impl CrabPortTerminal for FailedPtyBackend {
    fn write(&self, _data: &[u8]) {}
    fn resize(&self, _cols: u16, _rows: u16) {}
    fn close(&self) {}
    fn subscribe(&self) -> BroadcastReceiver<BackendEvent> {
        self.event_tx.new_receiver()
    }
    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        Some(self)
    }
    fn allow_history(&self) -> bool {
        false
    }
    fn allow_snippets(&self) -> bool {
        false
    }
    fn spawn_channel(
        &self,
        _cols: u16,
        _rows: u16,
    ) -> Option<std::sync::Arc<dyn CrabPortTerminal>> {
        None
    }
}

impl CrabPortMonitor for FailedPtyBackend {
    fn status(&self) -> RemoteStatus {
        RemoteStatus::Disconnected
    }
    fn metrics(&self) -> RemoteMetrics {
        RemoteMetrics::default()
    }
}

// ---------------------------------------------------------------------------
// Local shell-history reading
// ---------------------------------------------------------------------------

/// Maximum number of history entries to surface in the UI panel. Mirrors
/// the SSH-side cap so local + remote behave the same.
#[cfg(not(windows))]
const MAX_LOCAL_HISTORY: usize = 1000;

/// Read the local user's shell history file and return its commands,
/// most-recent-first.
///
/// We pick the file based on `$SHELL` (zsh → `~/.zsh_history`, bash →
/// `~/.bash_history`), falling back to whichever of those exists. This
/// isn't perfectly accurate (the user might run a different shell inside
/// the PTY than `$SHELL` claims), but it covers the common case.
///
/// On Windows there is no equivalent shell history file (PowerShell stores
/// its history in a registry / module-specific location, and `cmd.exe` has
/// none), so we return an empty list and rely on the session's own
/// in-memory capture.
fn read_local_shell_history() -> Vec<String> {
    #[cfg(windows)]
    {
        return Vec::new();
    }
    #[cfg(not(windows))]
    {
        let home = match std::env::var("HOME") {
            Ok(h) => std::path::PathBuf::from(h),
            Err(_) => return Vec::new(),
        };
        let shell = std::env::var("SHELL").unwrap_or_default();
        let candidates: Vec<std::path::PathBuf> = if shell.contains("zsh") {
            vec![home.join(".zsh_history"), home.join(".bash_history")]
        } else if shell.contains("bash") {
            vec![home.join(".bash_history"), home.join(".zsh_history")]
        } else {
            // Unknown shell — try both, preferring whichever is larger.
            vec![home.join(".zsh_history"), home.join(".bash_history")]
        };

        for path in &candidates {
            if let Ok(contents) = std::fs::read_to_string(path) {
                return parse_local_shell_history(&contents);
            }
        }
        Vec::new()
    }
}

/// Parse the contents of a local shell history file. Same logic as the
/// SSH-side parser (see `crabport_ssh::terminal::parse_shell_history`):
/// zsh extended format is `: <ts>:<dur>;<command>` with possibly multi-line
/// commands; bash is plain one-command-per-line.
#[cfg(not(windows))]
fn parse_local_shell_history(raw: &str) -> Vec<String> {
    let mut cmds: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix(':') {
            if let Some(semicolon) = rest.find(';') {
                if let Some(c) = current.take() {
                    push_local_cmd(&mut cmds, c);
                }
                current = Some(rest[semicolon + 1..].to_string());
                continue;
            }
            // Malformed meta line — fall through to plain handling.
        }
        if let Some(c) = current.as_mut() {
            c.push('\n');
            c.push_str(line);
        } else {
            push_local_cmd(&mut cmds, line.to_string());
        }
    }
    if let Some(c) = current.take() {
        push_local_cmd(&mut cmds, c);
    }
    // File is oldest-first; UI wants most-recent-first.
    cmds.reverse();
    if cmds.len() > MAX_LOCAL_HISTORY {
        cmds.truncate(MAX_LOCAL_HISTORY);
    }
    cmds
}

#[cfg(not(windows))]
fn push_local_cmd(out: &mut Vec<String>, s: String) {
    let trimmed = s.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
}
