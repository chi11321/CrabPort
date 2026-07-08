//! Local PTY backend.
//!
//! Fully implemented on Unix (`#[cfg(unix)]`) where `alacritty_terminal`'s
//! `tty::Pty` exposes `file()` / `child()` and we can drive `ioctl(TIOCSWINSZ)`
//! + `waitpid` directly.
//!
//! On Windows (`#[cfg(windows)]`) this module compiles but is a **stub**:
//! `PtyBackend::new` always returns an error. The alacritty `Pty` API on
//! Windows differs significantly (ConPTY, no `file()`/`child()` accessors,
//! resize goes through the `OnResize` trait) and is not wired up here. Callers
//! that hit this path on Windows will surface the error — remote SSH sessions
//! (the app's primary use case) are unaffected since they use `SshBackend`.

// Imports used only by the Unix implementation. Everything shared across
// platforms (struct fields, trait impls) is imported unconditionally below.
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
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

#[cfg(unix)]
use alacritty_terminal::{
    event::WindowSize,
    tty::{self, Options, Pty},
};
#[cfg(unix)]
use async_broadcast::broadcast;
// `BroadcastReceiver` is the return type of `CrabPortTerminal::subscribe`,
// which is implemented for all platforms (including the Windows stub).
use async_broadcast::Receiver as BroadcastReceiver;
use async_channel::{Sender as MpscSender, unbounded};
#[cfg(unix)]
use libc::{TIOCSWINSZ, ioctl, winsize};
use parking_lot::{Mutex, RwLock};

use crate::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, MemoryStats, NetworkStats, RemoteMetrics,
    RemoteStatus,
};

pub struct PtyBackend {
    #[cfg(unix)]
    _pty: Arc<Mutex<Pty>>,
    command_tx: MpscSender<Command>,
    event_tx: async_broadcast::Sender<BackendEvent>,
    #[cfg(unix)]
    _event_rx: async_broadcast::InactiveReceiver<BackendEvent>,
    sys: RwLock<sysinfo::System>,
    networks: RwLock<sysinfo::Networks>,
    /// Monotonic millis of the last sysinfo refresh.
    last_refresh_ms: AtomicU64,
    /// Cached metrics snapshot.
    cached_metrics: RwLock<RemoteMetrics>,
    /// Previous cumulative network bytes (for computing per-second rate).
    prev_net_sent: AtomicU64,
    prev_net_recv: AtomicU64,
}

enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

impl PtyBackend {
    #[cfg(unix)]
    pub fn new(cols: u16, rows: u16) -> std::io::Result<Self> {
        tty::setup_env();

        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 0,
            cell_height: 0,
        };

        let pty = Arc::new(Mutex::new(tty::new(&Options::default(), window_size, 0)?));

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
                            #[cfg(debug_assertions)]
                            tracing::info!("pty reader: EOF");
                            let _ = smol::block_on(event_tx.broadcast(BackendEvent::Closed));
                            break;
                        }

                        Ok(n) => {
                            #[cfg(debug_assertions)]
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
            sys: RwLock::new(sysinfo::System::new()),
            networks: RwLock::new(sysinfo::Networks::new_with_refreshed_list()),
            last_refresh_ms: AtomicU64::new(0),
            cached_metrics: RwLock::new(RemoteMetrics::default()),
            prev_net_sent: AtomicU64::new(0),
            prev_net_recv: AtomicU64::new(0),
        })
    }

    /// Windows stub: local PTY is not supported on Windows. Remote SSH
    /// sessions are unaffected (they use `SshBackend`, not `PtyBackend`).
    #[cfg(not(unix))]
    pub fn new(_cols: u16, _rows: u16) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "local PTY backend is not supported on Windows; use a remote SSH session instead",
        ))
    }
}

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
}

impl CrabPortMonitor for PtyBackend {
    fn status(&self) -> RemoteStatus {
        RemoteStatus::Local
    }

    fn metrics(&self) -> RemoteMetrics {
        // Refresh at most once per second; first call always refreshes
        // because last_refresh_ms starts at 0.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last = self.last_refresh_ms.load(AtomicOrdering::Relaxed);
        if now.saturating_sub(last) >= 1000 {
            // Only one writer wins the race
            if self
                .last_refresh_ms
                .compare_exchange(last, now, AtomicOrdering::Relaxed, AtomicOrdering::Relaxed)
                .is_ok()
            {
                {
                    let mut sys = self.sys.write();
                    sys.refresh_memory();
                }
                {
                    let mut networks = self.networks.write();
                    networks.refresh(true);
                }

                let sys = self.sys.read();
                let memory = MemoryStats {
                    total: sys.total_memory(),
                    used: sys.used_memory(),
                };
                drop(sys);

                let networks = self.networks.read();
                let mut bytes_sent: u64 = 0;
                let mut bytes_recv: u64 = 0;
                for (_name, network) in networks.iter() {
                    bytes_sent += network.transmitted();
                    bytes_recv += network.received();
                }

                // Compute per-second rate from cumulative delta
                let prev_sent = self.prev_net_sent.swap(bytes_sent, AtomicOrdering::Relaxed);
                let prev_recv = self.prev_net_recv.swap(bytes_recv, AtomicOrdering::Relaxed);
                let rate_sent = bytes_sent.saturating_sub(prev_sent);
                let rate_recv = bytes_recv.saturating_sub(prev_recv);

                let network = NetworkStats {
                    bytes_sent: rate_sent,
                    bytes_recv: rate_recv,
                };

                let mut cached = self.cached_metrics.write();
                *cached = RemoteMetrics {
                    latency_ms: None,
                    memory: Some(memory),
                    network: Some(network),
                };
            }
        }

        self.cached_metrics.read().clone()
    }
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}
