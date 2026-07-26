use crabport_core::credential::ProxyConfig;

/// One hop of a jump-host (bastion) chain.
///
/// Carries everything needed to connect + authenticate to a single jump
/// host: the SSH endpoint, credentials (password or private key), and an
/// optional proxy for the hop's own TCP connection. Resolved by the UI from
/// a saved host entry (`HostEntry.jump_host_id`) at connect time.
#[derive(Debug, Clone)]
pub struct JumpHostInfo {
    /// Jump host name or IP address.
    pub host: String,
    /// SSH port of the jump host.
    pub port: u16,
    /// Login username on the jump host.
    pub username: String,
    /// Password for password authentication.
    pub password: String,
    /// Private key for key-based authentication (PEM content or file path;
    /// resolved by [`crate::keys::decode_private_key`]).
    pub private_key: Option<String>,
    /// Passphrase for the private key (if encrypted).
    pub passphrase: Option<String>,
    /// Optional proxy for this hop's own TCP connection. Only meaningful
    /// for the FIRST hop in a chain — later hops ride a `direct-tcpip`
    /// channel of the previous hop, so no raw TCP connection is made.
    pub proxy: Option<ProxyConfig>,
}

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
    /// Commands to run automatically once the SSH shell is ready. Each line
    /// is sent verbatim followed by `\r`. Empty string means no startup
    /// command.
    pub startup_command: String,
    /// Jump-host (bastion) chain to reach the target through, in connection
    /// order: `jump_hosts[0]` is the first hop we TCP-connect to; each
    /// subsequent hop (and finally the target) is reached via a
    /// `direct-tcpip` channel opened on the previous hop's session. Empty
    /// means a direct connection (possibly through `proxy`). When
    /// non-empty, `proxy` applies to the TARGET handshake only if the
    /// first hop has no proxy of its own — in practice the UI sets the
    /// first hop's proxy and leaves this one for direct use.
    pub jump_hosts: Vec<JumpHostInfo>,
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
            startup_command: String::new(),
            jump_hosts: Vec::new(),
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

    /// Set the startup command to run once the shell is ready.
    pub fn with_startup_command(mut self, command: impl Into<String>) -> Self {
        self.startup_command = command.into();
        self
    }

    /// Route the connection through a chain of jump hosts (bastions).
    /// `jump_hosts[0]` is the first hop (the one we TCP-connect to).
    pub fn with_jump_hosts(mut self, jump_hosts: Vec<JumpHostInfo>) -> Self {
        self.jump_hosts = jump_hosts;
        self
    }

    /// Returns true if this connection should use key-based auth.
    pub fn uses_key_auth(&self) -> bool {
        self.private_key.is_some()
    }
}
