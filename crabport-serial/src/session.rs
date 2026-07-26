/// Connection parameters for a Serial session.
///
/// Unlike SSH/Telnet, serial has no network host or authentication. The
/// "host" field here stores the serial device path (e.g. "/dev/ttyUSB0"
/// on Unix, "COM3" on Windows). The `port` field is unused (kept for
/// structural parity with HostEntry). Username/password are unused but
/// kept for HostEntry structural parity.
#[derive(Debug, Clone)]
pub struct SerialConnectionInfo {
    /// Serial device path (e.g. "/dev/ttyUSB0", "COM3").
    pub device: String,
    /// Baud rate (e.g. 115200). Default 115200.
    pub baud_rate: u32,
    /// Data bits: 5, 6, 7, or 8. Default 8.
    pub data_bits: u8,
    /// Parity: "none", "odd", or "even". Default "none".
    pub parity: String,
    /// Stop bits: 1 or 2. Default 1.
    pub stop_bits: u8,
    /// Flow control: "none", "software", or "hardware". Default "none".
    pub flow_control: String,
    /// Commands to run automatically once the serial connection is
    /// established. Each line is sent verbatim followed by `\r`.
    pub startup_command: String,
}

impl SerialConnectionInfo {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            baud_rate: 115200,
            data_bits: 8,
            parity: "none".to_string(),
            stop_bits: 1,
            flow_control: "none".to_string(),
            startup_command: String::new(),
        }
    }

    pub fn with_baud_rate(mut self, baud: u32) -> Self {
        self.baud_rate = baud;
        self
    }
    pub fn with_data_bits(mut self, bits: u8) -> Self {
        self.data_bits = bits;
        self
    }
    pub fn with_parity(mut self, parity: impl Into<String>) -> Self {
        self.parity = parity.into();
        self
    }
    pub fn with_stop_bits(mut self, bits: u8) -> Self {
        self.stop_bits = bits;
        self
    }
    pub fn with_flow_control(mut self, fc: impl Into<String>) -> Self {
        self.flow_control = fc.into();
        self
    }
    pub fn with_startup_command(mut self, cmd: impl Into<String>) -> Self {
        self.startup_command = cmd.into();
        self
    }
}
