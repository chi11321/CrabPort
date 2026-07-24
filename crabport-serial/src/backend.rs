//! `SerialBackend` — serial port transport.
//!
//! Opens a local serial port via the `serialport` crate and bridges the
//! raw byte stream to the frontend. No protocol negotiation, no
//! authentication — just raw bytes in both directions.
//!
//! ## Threading model
//!
//! `serialport::SerialPort` is a *blocking* `std::io::Read + Write`. It
//! cannot be driven by tokio's async I/O. The standard pattern (and the
//! one used here) is to own the port on a dedicated OS thread that runs a
//! poll-based read+write loop:
//!
//! - **Reads** use the port's short (10 ms) timeout so the loop wakes
//!   frequently and can check for incoming commands.
//! - **Writes** are drained from an `async_channel` receiver via
//!   `try_recv()` (non-blocking) each loop iteration.
//!
//! A pair of `std::sync::mpsc` channels carries data/closed signals from
//! the reader thread back to a lightweight tokio task, which broadcasts
//! `BackendEvent`s to the frontend. This keeps the blocking I/O off the
//! tokio runtime while still using async broadcast for delivery.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender, broadcast};
use async_channel::{Sender as MpscSender, unbounded};
use parking_lot::RwLock;
use tokio::runtime::Runtime;

use crabport_terminal::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, RemoteMetrics, RemoteStatus,
};

use crate::session::SerialConnectionInfo;

/// Tokio runtime shared by all serial backends. The serialport crate's
/// I/O is blocking, so we run reads on a dedicated OS thread and bridge
/// to async via the broadcaster task spawned here.
pub static TOKIO: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("failed to create tokio runtime for serial"));

#[derive(Debug)]
enum Command {
    Write(Vec<u8>),
    Close,
}

struct MonitorState {
    status: RemoteStatus,
    metrics: RemoteMetrics,
}

pub struct SerialBackend {
    command_tx: MpscSender<Command>,
    event_tx: BroadcastSender<BackendEvent>,
    _event_rx: InactiveReceiver<BackendEvent>,
    monitor: Arc<RwLock<MonitorState>>,
    #[allow(dead_code)]
    _on_status: Arc<dyn Fn(String) + Send + Sync>,
}

impl SerialBackend {
    /// Create and open a serial backend.
    ///
    /// `on_status` receives human-readable connection-state updates
    /// ("Opening serial port …", "Connected to … @ … baud", …) mirroring
    /// `SshBackend::new` / `TelnetBackend::new`.
    pub fn new(info: SerialConnectionInfo, on_status: Arc<dyn Fn(String) + Send + Sync>) -> Self {
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
            on_status2(format!("Opening serial port {}", info.device));

            let port = {
                let s = serialport::new(&info.device, info.baud_rate)
                    .data_bits(data_bits_from(info.data_bits))
                    .parity(parity_from(&info.parity))
                    .stop_bits(stop_bits_from(info.stop_bits))
                    .flow_control(flow_control_from(&info.flow_control))
                    .timeout(Duration::from_millis(10));
                match s.open() {
                    Ok(p) => {
                        tracing::info!("serial: opened {} @ {} baud", info.device, info.baud_rate);
                        on_status2(format!(
                            "Connected to {} @ {} baud",
                            info.device, info.baud_rate
                        ));
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Connected;
                        }
                        let _ = event_tx2.broadcast(BackendEvent::Ready).await;
                        p
                    }
                    Err(e) => {
                        tracing::error!("serial: open failed: {e}");
                        on_status2(format!("Failed to open {}: {e}", info.device));
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
            };

            // Spawn a dedicated OS thread for the blocking read+write loop.
            // `serialport::SerialPort` is a blocking `Read+Write` — it can't
            // be used with tokio's async I/O. A dedicated thread is the
            // standard approach (mirrors how serial terminal apps work).
            let (data_tx, data_rx) = std::sync::mpsc::channel::<Vec<u8>>();
            let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
            let startup_cmd = info.startup_command.clone();

            std::thread::Builder::new()
                .name("crabport-serial-io".into())
                .spawn(move || {
                    let mut port = port;
                    let mut buf = [0u8; 1024];

                    // Send startup command immediately after open, before the
                    // read loop starts, so the device sees it first.
                    if !startup_cmd.is_empty() {
                        let payload =
                            crabport_core::credential::build_startup_command_bytes(&startup_cmd);
                        if !payload.is_empty() {
                            tracing::info!(
                                "serial: sending startup command ({} bytes)",
                                payload.len()
                            );
                            if let Err(e) = port.write_all(&payload) {
                                tracing::warn!("serial: startup command write error: {e}");
                            }
                            let _ = port.flush();
                        }
                    }

                    loop {
                        // Drain pending commands (non-blocking).
                        while let Ok(cmd) = command_rx.try_recv() {
                            match cmd {
                                Command::Write(data) => {
                                    if let Err(e) = port.write_all(&data) {
                                        tracing::warn!("serial: write error: {e}");
                                    }
                                    let _ = port.flush();
                                }
                                Command::Close => {
                                    tracing::info!("serial: closing port");
                                    let _ = closed_tx.send(());
                                    return;
                                }
                            }
                        }

                        // Read with the port's 10 ms timeout.
                        match port.read(&mut buf) {
                            Ok(0) => {
                                tracing::info!("serial: port closed (EOF)");
                                let _ = closed_tx.send(());
                                return;
                            }
                            Ok(n) => {
                                if data_tx.send(buf[..n].to_vec()).is_err() {
                                    // Broadcaster gone — exit.
                                    let _ = closed_tx.send(());
                                    return;
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                                // Normal — no data arrived in 10 ms.
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            Err(e) => {
                                tracing::error!("serial: read error: {e}");
                                let _ = closed_tx.send(());
                                return;
                            }
                        }
                    }
                })
                .expect("failed to spawn serial I/O thread");

            // Broadcaster: drain the reader thread's mpsc and broadcast to
            // the frontend. This runs on the tokio runtime so we can use
            // async broadcast. The 5 ms sleep on empty keeps CPU usage low
            // (serial is low-bandwidth).
            loop {
                match data_rx.try_recv() {
                    Ok(data) => {
                        let _ = event_tx2.broadcast(BackendEvent::Data(data)).await;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // Reader thread ended.
                        {
                            let mut m = monitor2.write();
                            m.status = RemoteStatus::Disconnected;
                        }
                        let _ = event_tx2.broadcast(BackendEvent::Closed).await;
                        return;
                    }
                }

                if closed_rx.try_recv().is_ok() {
                    {
                        let mut m = monitor2.write();
                        m.status = RemoteStatus::Disconnected;
                    }
                    let _ = event_tx2.broadcast(BackendEvent::Closed).await;
                    return;
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
// Config → serialport enum conversion helpers
// ---------------------------------------------------------------------------

fn data_bits_from(bits: u8) -> serialport::DataBits {
    match bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    }
}

fn parity_from(p: &str) -> serialport::Parity {
    match p.to_ascii_lowercase().as_str() {
        "odd" => serialport::Parity::Odd,
        "even" => serialport::Parity::Even,
        _ => serialport::Parity::None,
    }
}

fn stop_bits_from(bits: u8) -> serialport::StopBits {
    match bits {
        2 => serialport::StopBits::Two,
        _ => serialport::StopBits::One,
    }
}

fn flow_control_from(fc: &str) -> serialport::FlowControl {
    match fc.to_ascii_lowercase().as_str() {
        "software" => serialport::FlowControl::Software,
        "hardware" => serialport::FlowControl::Hardware,
        _ => serialport::FlowControl::None,
    }
}

// ---------------------------------------------------------------------------
// CrabPortTerminal impl
// ---------------------------------------------------------------------------

impl CrabPortTerminal for SerialBackend {
    fn write(&self, data: &[u8]) {
        let _ = self.command_tx.try_send(Command::Write(data.to_vec()));
    }

    fn resize(&self, _cols: u16, _rows: u16) {
        // Serial ports have no concept of terminal size — no-op.
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

impl CrabPortMonitor for SerialBackend {
    fn status(&self) -> RemoteStatus {
        self.monitor.read().status
    }

    fn metrics(&self) -> RemoteMetrics {
        self.monitor.read().metrics
    }
}

impl Drop for SerialBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}
