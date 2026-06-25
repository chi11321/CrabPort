use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    sync::Arc,
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
use parking_lot::Mutex;

use crate::terminal::{BackendEvent, CrabPortTerminal};

pub struct PtyBackend {
    _pty: Arc<Mutex<Pty>>,
    command_tx: MpscSender<Command>,
    event_tx: BroadcastSender<BackendEvent>,
    _event_rx: InactiveReceiver<BackendEvent>,
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
                            #[cfg(debug_assertions)]
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
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}
