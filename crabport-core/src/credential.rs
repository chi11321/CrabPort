use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

/// Proxy configuration for an SSH connection.
///
/// Stored on the host entry so it follows the session. The proxy is used
/// to tunnel the TCP connection to the SSH server — the SSH handshake
/// itself runs over the tunnelled stream, so encryption/auth are
/// unaffected by the proxy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Proxy protocol.
    pub kind: ProxyKind,
    /// `host:port` of the proxy server.
    pub host: String,
    /// Port of the proxy server.
    pub port: u16,
    /// Optional username for proxy auth (SOCKS5 user/pass or HTTP
    /// Proxy-Authorization Basic). `None` means no auth.
    #[serde(default)]
    pub username: Option<String>,
    /// Optional password for proxy auth. `None` means no auth.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyKind {
    #[default]
    None,
    Socks5,
    Http,
    Https,
}

impl ProxyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProxyKind::None => "none",
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "socks5" | "socks" => ProxyKind::Socks5,
            "http" => ProxyKind::Http,
            "https" => ProxyKind::Https,
            _ => ProxyKind::None,
        }
    }
}

impl ProxyConfig {
    /// Returns `true` if a proxy is actually configured (kind != None and
    /// host is non-empty).
    pub fn is_enabled(&self) -> bool {
        self.kind != ProxyKind::None && !self.host.is_empty()
    }

    /// Render this config back into the URL form used by the connection form
    /// (`scheme://[user[:pass]@]host:port`). Returns an empty string when the
    /// proxy is not enabled (kind == None or host empty).
    pub fn to_url(&self) -> String {
        if !self.is_enabled() {
            return String::new();
        }
        let scheme = self.kind.as_str();
        match (&self.username, &self.password) {
            (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {
                format!("{scheme}://{u}:{p}@{}:{}", self.host, self.port)
            }
            (Some(u), _) if !u.is_empty() => {
                format!("{scheme}://{u}@{}:{}", self.host, self.port)
            }
            _ => format!("{scheme}://{}:{}", self.host, self.port),
        }
    }

    /// Detect a proxy from the standard environment variables
    /// (`ALL_PROXY` / `all_proxy`, then `HTTPS_PROXY` / `https_proxy`, then
    /// `HTTP_PROXY` / `http_proxy`). Returns `None` if nothing is set or the
    /// value can't be parsed.
    pub fn from_system() -> Option<Self> {
        for key in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            if let Ok(val) = std::env::var(key) {
                if let Some(cfg) = parse_proxy_url(&val) {
                    return Some(cfg);
                }
            }
        }
        None
    }
}

/// Parse a proxy URL string into a `ProxyConfig`.
///
/// Accepted formats:
///   `socks5://host:port`
///   `socks5://user:pass@host:port`
///   `http://host:port`
///   `https://user:pass@host:port`
///
/// Returns `None` if the URL is empty or unparseable. This lives at the
/// crate root (rather than on `ProxyConfig`) so callers can use it without
/// constructing an instance first.
pub fn parse_proxy_url(url: &str) -> Option<ProxyConfig> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // Split scheme://rest
    let (scheme_str, rest) = url.split_once("://")?;
    let kind = ProxyKind::from_str(scheme_str);
    if kind == ProxyKind::None {
        return None;
    }

    // Split user:pass@host:port
    let (auth, host_port) = if let Some((auth, hp)) = rest.rsplit_once('@') {
        (Some(auth), hp)
    } else {
        (None, rest)
    };

    // Split host:port
    let (host, port_str) = host_port.rsplit_once(':')?;
    let port: u16 = port_str.parse().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }

    let (username, password) = if let Some(auth) = auth {
        if let Some((u, p)) = auth.split_once(':') {
            (
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_string())
                },
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                },
            )
        } else {
            (Some(auth.to_string()), None)
        }
    } else {
        (None, None)
    };

    Some(ProxyConfig {
        kind,
        host: host.to_string(),
        port,
        username,
        password,
    })
}

/// A persisted proxy row in the `proxies` table.
///
/// `ProxyConfig` is the lightweight in-memory shape used at connect time;
/// `ProxyEntry` is the database row — it carries an `id`, a user-facing
/// `name`, and an encrypted `password` blob (decrypted only when building
/// a `ProxyConfig` for an actual connection).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyEntry {
    pub id: i64,
    pub name: String,
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    /// Optional username for proxy auth. `None` means no auth.
    #[serde(default)]
    pub username: Option<String>,
    /// Encrypted password blob (AES-256-GCM). `None` means no auth.
    #[serde(default)]
    pub password: Option<Vec<u8>>,
    #[serde(default)]
    pub created_at: i64,
}

impl ProxyEntry {
    /// Build the in-memory `ProxyConfig` used at connect time, decrypting
    /// the password via the Store's encryption key.
    pub fn to_config(&self, enc_key: &[u8]) -> Result<ProxyConfig, crate::crypto::CryptoError> {
        let password = match &self.password {
            Some(blob) if !blob.is_empty() => {
                Some(String::from_utf8_lossy(&crate::crypto::decrypt(blob, enc_key)?).into_owned())
            }
            _ => None,
        };
        Ok(ProxyConfig {
            kind: self.kind,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password,
        })
    }
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostEntry {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credential_id: Option<i64>,
    pub kind: HostKind,
    #[serde(default)]
    pub last_login: Option<i64>,
    #[serde(default)]
    pub favorite: bool,
    /// Optional proxy to tunnel the TCP connection through. FK into the
    /// `proxies` table. `None` means direct connection.
    #[serde(default)]
    pub proxy_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKind {
    Ssh,
    Telnet,
    Serial,
}

impl HostKind {
    pub fn default_port(&self) -> u16 {
        match self {
            HostKind::Ssh => 22,
            HostKind::Telnet => 23,
            HostKind::Serial => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Credential
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub id: i64,
    pub name: String,
    pub kind: CredentialKind,
    /// Anonymous credentials are auto-created from the connection form
    /// and hidden from the credentials list.
    #[serde(default)]
    pub anonymous: bool,
    /// For Password kind: the password. For Certificate kind: the passphrase.
    /// Stored encrypted in SQLite; decrypted only in memory.
    pub secret: String,
    /// Certificate-only fields (empty strings when not applicable).
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub certificate: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    Password,
    Certificate,
}

// ---------------------------------------------------------------------------
// Snippet
// ---------------------------------------------------------------------------

/// A saved command snippet. Persisted globally (not scoped to a host) so
/// the user can build a reusable library of commands across connections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnippetEntry {
    pub id: i64,
    pub name: String,
    /// Literal command text to insert into the terminal.
    pub command: String,
    #[serde(default)]
    pub created_at: i64,
}
