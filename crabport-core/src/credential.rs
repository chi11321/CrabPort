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

    /// Detect a proxy from the environment, then (on macOS) from the
    /// system network preferences.
    ///
    /// Lookup order:
    /// 1. Environment variables — `ALL_PROXY` / `all_proxy`, then
    ///    `HTTPS_PROXY` / `https_proxy`, then `HTTP_PROXY` /
    ///    `http_proxy`. This matches the convention used by curl, git,
    ///    and most CLI tools, so an explicit env var always wins over
    ///    the OS network preferences.
    /// 2. (macOS only) System network preferences via `networksetup` —
    ///    i.e. the proxy configured in "System Settings → Network →
    ///    Proxies". The default network service is resolved from the
    ///    primary route's interface (`route -n get default`) and mapped
    ///    to its network-service name via `networksetup
    ///    -listallhardwareports`. If that resolution fails, every
    ///    non-disabled service is probed in order and the first enabled
    ///    proxy wins. Within a service, SOCKS is preferred over HTTPS,
    ///    which is preferred over HTTP — SOCKS5 tunnels any TCP, so it's
    ///    the most generally usable for SSH.
    ///
    /// Returns `None` if nothing is set or the value can't be parsed.
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

        #[cfg(target_os = "macos")]
        {
            if let Some(cfg) = from_macos_networksetup() {
                return Some(cfg);
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// macOS system proxy (networksetup)
// ---------------------------------------------------------------------------
//
// Reads the proxy configured in "System Settings → Network → Proxies" via
// the `networksetup` CLI. We shell out instead of linking against
// `system-configuration` to keep `crabport-core` free of platform-only
// native deps and to stay buildable on Linux/Windows without conditional
// linking.
//
// The lookup mirrors what browsers and curl-on-macOS do: SOCKS wins over
// HTTPS over HTTP within the *primary* network service (the one backing
// the default route). If the primary service can't be determined, every
// non-disabled service is probed in `listallnetworkservices` order and the
// first enabled proxy wins. Credentials aren't exposed by `networksetup`,
// so the returned `ProxyConfig` is always anonymous — callers needing auth
// should use the Custom proxy mode instead.
#[cfg(target_os = "macos")]
fn from_macos_networksetup() -> Option<ProxyConfig> {
    use std::process::Command;

    /// Probe a single `networksetup -get<target>` subcommand for a service.
    /// Returns a `ProxyConfig` when the proxy is enabled and the host/port
    /// parse cleanly; `None` otherwise (disabled / unparseable).
    ///
    /// Output shape (with `\n` line endings):
    ///   ```text
    ///   Enabled: Yes
    ///   Server: 127.0.0.1
    ///   Port: 7897
    ///   Authenticated Proxy Enabled: 0
    ///   ```
    fn probe(subcmd: &str, kind: ProxyKind, service: &str) -> Option<ProxyConfig> {
        let out = Command::new("networksetup")
            .args([format!("-get{subcmd}"), service.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        if field(&s, "Enabled:") != "Yes" {
            return None;
        }
        let host = field(&s, "Server:");
        if host.is_empty() {
            return None;
        }
        let port: u16 = field(&s, "Port:").parse().ok()?;
        if port == 0 {
            return None;
        }
        Some(ProxyConfig {
            kind,
            host: host.to_string(),
            port,
            username: None,
            password: None,
        })
    }

    /// Extract the value following `key:` on its own line, trimmed.
    fn field<'a>(output: &'a str, key: &str) -> &'a str {
        for line in output.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(key) {
                return rest.trim();
            }
        }
        ""
    }

    /// Resolve the network service name backing the default route.
    ///
    /// `route -n get default` → interface (e.g. `en0`) →
    /// `networksetup -listallhardwareports` maps device → service name.
    /// Falls back to scanning every non-disabled service listed by
    /// `networksetup -listallnetworkservices` if the route can't be
    /// resolved (e.g. no default route, or interface not found).
    fn primary_service() -> Option<String> {
        if let Some(dev) = default_interface() {
            if let Some(svc) = service_for_device(&dev) {
                return Some(svc);
            }
        }
        first_listed_service()
    }

    /// `route -n get default` → `interface: en0` → `en0`.
    fn default_interface() -> Option<String> {
        let out = Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("interface:") {
                let v = rest.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        None
    }

    /// Walk `networksetup -listallhardwareports` to map a device name
    /// (e.g. `en0`) to its service name (e.g. `Wi-Fi`).
    ///
    /// Each block looks like:
    ///   ```text
    ///   Hardware Port: Wi-Fi
    ///   Device: en0
    ///   Ethernet Address: ...
    ///   ```
    fn service_for_device(device: &str) -> Option<String> {
        let out = Command::new("networksetup")
            .arg("-listallhardwareports")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let mut current_hw: Option<String> = None;
        for line in s.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Hardware Port:") {
                current_hw = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("Device:") {
                if rest.trim() == device {
                    return current_hw.take();
                }
            }
        }
        None
    }

    /// First non-disabled service from `networksetup -listallnetworkservices`.
    /// The first line of that output is a banner; lines starting with `*`
    /// denote disabled services and are skipped.
    fn first_listed_service() -> Option<String> {
        let out = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let mut lines = s.lines();
        lines.next(); // banner
        for line in lines {
            let line = line.trim();
            if line.is_empty() || line.starts_with('*') {
                continue;
            }
            return Some(line.to_string());
        }
        None
    }

    /// All non-disabled services from `networksetup -listallnetworkservices`,
    /// used when the primary service can't be resolved.
    fn all_services() -> Vec<String> {
        let out = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .ok();
        let s = match out {
            Some(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            None => return Vec::new(),
        };
        let mut lines = s.lines();
        lines.next(); // banner
        lines
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('*'))
            .map(|l| l.to_string())
            .collect()
    }

    // Prefer SOCKS → HTTPS → HTTP, mirroring how a SOCKS-capable proxy is
    // the most generally usable for tunneling arbitrary TCP (SSH).
    const PROBES: &[(&str, ProxyKind)] = &[
        ("socksfirewallproxy", ProxyKind::Socks5),
        ("securewebproxy", ProxyKind::Https),
        ("webproxy", ProxyKind::Http),
    ];

    // Try the primary service first, then fall back to scanning every
    // non-disabled service. The primary-service fast path avoids probing
    // 3 subcommands × N services on every connect.
    let mut services = Vec::new();
    if let Some(svc) = primary_service() {
        services.push(svc);
    }
    for s in all_services() {
        if !services.contains(&s) {
            services.push(s);
        }
    }

    for service in &services {
        for (subcmd, kind) in PROBES {
            if let Some(cfg) = probe(subcmd, *kind, service) {
                return Some(cfg);
            }
        }
    }

    None
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
    /// User-assigned group id for organizing hosts in the sessions list.
    /// FK into the `groups` table. `None` means "ungrouped" (shown at the
    /// top level).
    #[serde(default)]
    pub group_id: Option<i64>,
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
    /// How [`private_key`] should be interpreted for Certificate credentials:
    /// the literal PEM key material (`Content`) or a filesystem path to a
    /// key file (`Path`). Ignored for Password credentials. Defaults to
    /// `Content` so pre-existing rows (which store pasted PEM) keep working.
    #[serde(default)]
    pub private_key_kind: PrivateKeyKind,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub certificate: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PrivateKeyKind {
    /// `private_key` holds the literal PEM key material.
    #[default]
    Content,
    /// `private_key` holds a filesystem path to a key file.
    Path,
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
    /// Starred by the user to pin it above un-starred snippets. Stored as
    /// an INTEGER (0/1) in SQLite.
    #[serde(default)]
    pub favorite: bool,
    /// User-assigned group id for organizing snippets. FK into the `groups`
    /// table. `None` means "ungrouped" (shown at the top level).
    #[serde(default)]
    pub group_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Tunnel
// ---------------------------------------------------------------------------

/// Which kind of SSH port forwarding a tunnel represents.
/// Mirrors `ssh -L` (Local), `ssh -R` (Remote), `ssh -D` (Dynamic/SOCKS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelKind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl TunnelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelKind::Local => "local",
            TunnelKind::Remote => "remote",
            TunnelKind::Dynamic => "dynamic",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "remote" => TunnelKind::Remote,
            "dynamic" => TunnelKind::Dynamic,
            _ => TunnelKind::Local,
        }
    }
}

/// A persisted tunnel configuration. Bound to a host via `host_id`.
/// The actual SSH connection is established at start time — either a fresh
/// independent connection (started from Tunnels page) or by borrowing an
/// already-connected tab's session (started from a terminal panel).
///
/// Field semantics by kind (matches `ssh -L/-R/-D`):
/// - **Local** (`-L bind_addr:bind_port:target_host:target_port`): listen
///   locally on `bind_addr:bind_port`, forward to `target_host:target_port`
///   via the SSH server.
/// - **Remote** (`-R bind_addr:bind_port:target_host:target_port`): listen on
///   the SSH server at `bind_addr:bind_port`, forward to
///   `target_host:target_port` on the local machine.
/// - **Dynamic** (`-D bind_addr:bind_port`): listen locally on
///   `bind_addr:bind_port` as a SOCKS5 proxy; `target_host`/`target_port`
///   unused.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TunnelEntry {
    pub id: i64,
    pub name: String,
    /// FK -> hosts.id
    pub host_id: i64,
    pub kind: TunnelKind,
    /// Local/Remote: the host side bind address. Empty = all interfaces
    /// (0.0.0.0). Dynamic: the local SOCKS bind address.
    #[serde(default)]
    pub bind_addr: String,
    /// Local/Remote: the host side port (local listen for Local, remote
    /// listen for Remote). Dynamic: the local SOCKS listen port.
    #[serde(default)]
    pub bind_port: u16,
    /// Local/Remote only: the target host to forward to. For Local this is
    /// reachable from the SSH server; for Remote this is reachable from the
    /// local machine. Empty for Dynamic.
    #[serde(default)]
    pub target_host: String,
    /// Local/Remote only: the target port. 0 for Dynamic.
    #[serde(default)]
    pub target_port: u16,
    #[serde(default)]
    pub created_at: i64,
    /// Starred by the user to pin it above un-starred tunnels. Stored as
    /// an INTEGER (0/1) in SQLite.
    #[serde(default)]
    pub favorite: bool,
    /// User-assigned group id for organizing tunnels. FK into the `groups`
    /// table. `None` means "ungrouped" (shown at the top level).
    #[serde(default)]
    pub group_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Group
// ---------------------------------------------------------------------------

/// Which collection a group organizes. The `groups` table is shared across
/// all three collections (hosts / snippets / tunnels) so this discriminates
/// which list a group belongs to — a "Production" host group is distinct
/// from a "Production" snippet group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    #[default]
    Host,
    Snippet,
    Tunnel,
}

impl GroupKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GroupKind::Host => "host",
            GroupKind::Snippet => "snippet",
            GroupKind::Tunnel => "tunnel",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "snippet" => GroupKind::Snippet,
            "tunnel" => GroupKind::Tunnel,
            _ => GroupKind::Host,
        }
    }
}

/// A user-created group for organizing hosts / snippets / tunnels in their
/// respective management views. `kind` scopes the group to one collection so
/// the same name can be reused across collections without collision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupEntry {
    pub id: i64,
    pub name: String,
    /// Which collection this group belongs to (see [`GroupKind`]).
    pub kind: GroupKind,
    /// Display ordering within a collection's group list. Lower sorts first.
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    /// Starred by the user to pin the group above non-starred groups.
    #[serde(default)]
    pub favorite: bool,
}
