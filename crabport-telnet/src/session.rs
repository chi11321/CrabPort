use crabport_core::credential::ProxyConfig;

/// Connection parameters for a Telnet session.
///
/// Mirrors [`crabport_ssh::session::SshConnectionInfo`] but drops the SSH-only
/// key/auth fields. Telnet has no real authentication handshake — the server
/// sends `login:` / `Password:` prompts that the user types into the terminal —
/// so `username` / `password` here are stored only for future auto-login
/// support and are not sent on connect in v1.
#[derive(Debug, Clone)]
pub struct TelnetConnectionInfo {
    /// Remote hostname or IP address.
    pub host: String,
    /// Telnet port (default: 23).
    pub port: u16,
    /// Login username (not auto-sent in v1; kept for future auto-login).
    pub username: String,
    /// Login password (not auto-sent in v1; kept for future auto-login).
    pub password: String,
    /// Optional proxy to tunnel the TCP connection through. When set, the
    /// TCP connection goes to the proxy first, then the proxy establishes a
    /// tunnel to `host:port`, and the raw Telnet byte stream runs over that
    /// tunnelled stream.
    pub proxy: Option<ProxyConfig>,
    /// Commands to run automatically after the TCP connection is established
    /// and login completes (if auto-login sends credentials). Each line is
    /// sent verbatim followed by `\r`. Empty string means no startup command.
    pub startup_command: String,
}

impl TelnetConnectionInfo {
    /// Create a new connection info with the default telnet port (23).
    pub fn new(
        host: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port: 23,
            username: username.into(),
            password: password.into(),
            proxy: None,
            startup_command: String::new(),
        }
    }

    /// Set a custom telnet port.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Tunnel the TCP connection through a proxy (SOCKS5 / HTTP / HTTPS).
    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Set the startup command to run after connect.
    pub fn with_startup_command(mut self, command: impl Into<String>) -> Self {
        self.startup_command = command.into();
        self
    }
}
