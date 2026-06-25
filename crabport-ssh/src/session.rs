/// Connection parameters for an SSH session.
#[derive(Debug, Clone)]
pub struct SshConnectionInfo {
    /// Remote hostname or IP address.
    pub host: String,
    /// SSH port (default: 22).
    pub port: u16,
    /// Login username.
    pub username: String,
    /// Password for password authentication.
    pub password: String,
}

impl SshConnectionInfo {
    /// Create a new connection info with defaults.
    pub fn new(
        host: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port: 22,
            username: username.into(),
            password: password.into(),
        }
    }

    /// Set a custom SSH port.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}
