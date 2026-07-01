use crabport_core::credential::ProxyConfig;

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
    /// Private key for certificate/key-based authentication.
    pub private_key: Option<String>,
    /// Passphrase for the private key (if encrypted).
    pub passphrase: Option<String>,
    /// Optional proxy to tunnel the TCP connection through. When set, the
    /// SSH client connects to the proxy first, then the proxy establishes
    /// a tunnel to `host:port`, and the SSH handshake runs over that
    /// tunnelled stream.
    pub proxy: Option<ProxyConfig>,
}

impl SshConnectionInfo {
    /// Create a new connection info with password authentication.
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
            private_key: None,
            passphrase: None,
            proxy: None,
        }
    }

    /// Set a custom SSH port.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Use key-based authentication with an optional passphrase.
    pub fn with_private_key(
        mut self,
        private_key: impl Into<String>,
        passphrase: Option<String>,
    ) -> Self {
        self.private_key = Some(private_key.into());
        self.passphrase = passphrase;
        self
    }

    /// Tunnel the TCP connection through a proxy (SOCKS5 / HTTP / HTTPS).
    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Returns true if this connection should use key-based auth.
    pub fn uses_key_auth(&self) -> bool {
        self.private_key.is_some()
    }
}
