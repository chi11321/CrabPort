//! Serial terminal backend for CrabPort.
//!
//! Connects to a local serial port (e.g. /dev/ttyUSB0, COM3) and bridges
//! the byte stream to the frontend via the standard `CrabPortTerminal`
//! trait. No protocol negotiation — raw bytes in both directions.

pub mod backend;
pub mod session;

pub use backend::{SerialBackend, TOKIO};
pub use session::SerialConnectionInfo;

/// Enumerate available serial ports on the system.
///
/// Returns a list of `(port_name, description)` pairs. `port_name` is the
/// device path (e.g. `/dev/ttyUSB0`, `COM3`) used to open the port;
/// `description` is a human-readable label for the dropdown. Falls back
/// to just the port name when no richer info is available.
///
/// This is the only function that benefits from `libudev` on Linux —
/// without it, ports are still discovered via `/dev/tty*` scanning but
/// without USB metadata (manufacturer, product, serial number).
pub fn available_ports() -> Vec<(String, String)> {
    match serialport::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                let desc = match &p.port_type {
                    serialport::SerialPortType::UsbPort(usb) => {
                        let mut parts: Vec<String> = Vec::new();
                        if let Some(m) = &usb.manufacturer {
                            parts.push(m.clone());
                        }
                        if let Some(p) = &usb.product {
                            parts.push(p.clone());
                        }
                        if let Some(s) = &usb.serial_number {
                            parts.push(format!("SN: {s}"));
                        }
                        if parts.is_empty() {
                            p.port_name.clone()
                        } else {
                            format!("{} ({})", p.port_name, parts.join(" / "))
                        }
                    }
                    serialport::SerialPortType::BluetoothPort => {
                        format!("{} (Bluetooth)", p.port_name)
                    }
                    serialport::SerialPortType::PciPort => {
                        format!("{} (PCI)", p.port_name)
                    }
                    _ => p.port_name.clone(),
                };
                (p.port_name, desc)
            })
            .collect(),
        Err(e) => {
            tracing::warn!("serial: failed to enumerate ports: {e}");
            Vec::new()
        }
    }
}
