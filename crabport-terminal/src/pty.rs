use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    thread,
    time::Duration,
};

use alacritty_terminal::{
    event::WindowSize,
    tty::{self, Options, Pty},
};
use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};
use async_channel::{Sender as MpscSender, unbounded};
use libc::{TIOCSWINSZ, ioctl, winsize};
use parking_lot::{Mutex, RwLock};

use crate::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, MemoryStats, NetworkStats, RemoteMetrics,
    RemoteStatus,
};

pub struct PtyBackend {
    _pty: Arc<Mutex<Pty>>,
    command_tx: MpscSender<Command>,
    event_tx: BroadcastSender<BackendEvent>,
    _event_rx: InactiveReceiver<BackendEvent>,
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
