//! Tunnel execution layer for `crabport-ssh`.
//!
//! This module provides the [`CrabPortTunnel`] trait — an abstraction over "an
//! SSH session usable for tunneling" — and the [`TunnelManager`] that
//! implements the actual port-forwarding logic (Local `-L`, Remote `-R`, and
//! Dynamic `-D` SOCKS5) on top of any source implementing that trait.
//!
//! ## Sources
//!
//! There are two implementations of [`CrabPortTunnel`]:
//!
//! - [`crate::OwnedSession`]: a standalone SSH connection owned by the tunnel
//!   itself, used by the dedicated Tunnels page where the user wants a
//!   connection that exists purely for forwarding (no PTY / shell / SFTP).
//! - [`crate::SshBackend`]: reuses an already-connected terminal tab's session.
//!   The terminal's connection is borrowed, so tunnels started from a panel
//!   share that tab's lifecycle (closing the tab tears them down).
//!
//! ## Local (`-L`) tunnels
//!
//! `bind_addr:bind_port` is opened locally; each accepted connection is bridged
//! to `target_host:target_port` via a `direct-tcpip` SSH channel.
//!
//! ## Remote (`-R`) tunnels
//!
//! The SSH server is asked (via `tcpip_forward`) to listen on
//! `bind_addr:bind_port`; inbound connections there are reported back through
//! `SshHandler::server_channel_open_forwarded_tcpip`, which consults the shared
//! [`crate::ReverseForwardRegistry`] to find the local `host:port` to bridge
//! to. No local accept loop is needed.
//!
//! ## Dynamic (`-D`) tunnels
//!
//! `bind_addr:bind_port` is opened locally as a SOCKS5 proxy (no auth, CONNECT
//! only). For each CONNECT request we open a `direct-tcpip` channel to the
//! requested target and bridge. The SOCKS5 handshake is hand-rolled to avoid
//! pulling in a `tokio-socks` dependency.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;
use russh::client;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::AbortHandle;

use crabport_terminal::terminal::RemoteStatus;
// Transport-agnostic tunnel types (TunnelKind, TunnelId, TunnelInfo,
// TunnelStatus, ReverseForwardRegistry, LocalTarget) now live in the
// `crabport-tunnel` crate. We re-export them from `crate::` (see lib.rs)
// and pull the concrete ones we need here directly from the source crate.
use ::crabport_tunnel::{
    LocalTarget, ReverseForwardRegistry, TunnelId, TunnelInfo, TunnelKind, TunnelStatus,
};

use crate::handler::{SshHandler, bridge};

// ---------------------------------------------------------------------------
// CrabPortTunnel trait
// ---------------------------------------------------------------------------

/// A source of SSH sessions for tunneling.
///
/// Two implementations exist:
/// - [`crate::OwnedSession`]: establishes and owns its own SSH connection.
/// - [`crate::SshBackend`]: borrows an already-connected tab's session.
///
/// This abstraction lets [`TunnelManager`] run the same forward/reverse/dynamic
/// logic regardless of whether the tunnel was started from the Tunnels page
/// (owned connection) or from a terminal panel (borrowed connection).
#[async_trait::async_trait]
pub trait CrabPortTunnel: Send + Sync {
    /// Borrow the live SSH handle, if connected. Returns `None` when the
    /// session isn't established yet or has been dropped.
    async fn handle(&self) -> Option<Arc<TokioMutex<client::Handle<SshHandler>>>>;

    /// Current connection status (for UI display).
    fn status(&self) -> RemoteStatus;

    /// The reverse-forward registry. Remote tunnels register their
    /// (server-side bind addr:port -> local target) mapping here so the
    /// `server_channel_open_forwarded_tcpip` handler can dispatch incoming
    /// connections.
    fn reverse_registry(&self) -> Arc<ReverseForwardRegistry>;
}

// `TunnelId`, `TunnelStatus`, `TunnelInfo`, `TunnelKind`, `LocalTarget`, and
// `ReverseForwardRegistry` are re-exported from `crabport-tunnel` (see
// `lib.rs`). They used to be defined inline here but were hoisted to keep
// the transport-agnostic types in one place.

// ---------------------------------------------------------------------------
// Internal tunnel entry
// ---------------------------------------------------------------------------

/// Mutable per-tunnel state held under the manager's lock.
struct TunnelEntry {
    kind: TunnelKind,
    name: String,
    status: TunnelStatus,
    bind_addr: String,
    bind_port: u16,
    target_host: String,
    target_port: u16,
    bytes: u64,
    /// For Local/Dynamic tunnels: the abort handle of the accept-loop task.
    /// `None` for Remote tunnels (no accept loop — the handler dispatches).
    accept_task: Option<AbortHandle>,
    /// For Remote tunnels: the `(bind_addr, bind_port)` registered with the
    /// server via `tcpip_forward`, so `stop` can call `cancel_tcpip_forward`
    /// and remove the registry entry. `None` for Local/Dynamic.
    reverse_key: Option<(String, u32)>,
}

impl TunnelEntry {
    fn info(&self, id: TunnelId) -> TunnelInfo {
        TunnelInfo {
            id,
            kind: self.kind,
            name: self.name.clone(),
            status: self.status.clone(),
            bind_addr: self.bind_addr.clone(),
            bind_port: self.bind_port,
            target_host: self.target_host.clone(),
            target_port: self.target_port,
            bytes: self.bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// TunnelManager
// ---------------------------------------------------------------------------

/// Owns and drives a set of tunnels over a single [`CrabPortTunnel`] source.
///
/// The manager is cheap to clone-via-`Arc` (it's constructed once per source
/// and shared between the UI polling thread and the async accept loops). All
/// mutable state lives behind `parking_lot::Mutex`es, matching the convention
/// in `backend.rs`.
pub struct TunnelManager {
    source: Arc<dyn CrabPortTunnel>,
    tunnels: Arc<PlMutex<HashMap<TunnelId, TunnelEntry>>>,
    next_id: Arc<PlMutex<u64>>,
    /// Callback fired whenever tunnel state changes (a tunnel is added,
    /// transitions status, or is removed). The UI wires this to a
    /// `cx.notify()`-style refresh so the tunnels view re-renders.
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl TunnelManager {
    /// Construct a new manager backed by `source`. `on_change` is invoked
    /// (synchronously) on every state mutation; callers should keep it cheap
    /// (e.g. an `Arc<dyn Fn() + Send + Sync>` that increments a version /
    /// triggers a notify).
    pub fn new(source: Arc<dyn CrabPortTunnel>, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            source,
            tunnels: Arc::new(PlMutex::new(HashMap::new())),
            next_id: Arc::new(PlMutex::new(1)),
            on_change,
        }
    }

    fn alloc_id(&self) -> TunnelId {
        let mut next = self.next_id.lock();
        let id = *next;
        *next += 1;
        id
    }

    fn notify(&self) {
        (self.on_change)();
    }

    /// Start a Local (`-L`) tunnel.
    ///
    /// Listens on `bind_addr:bind_port` locally; each accepted connection is
    /// bridged to `target_host:target_port` over a `direct-tcpip` SSH channel.
    pub async fn start_local(
        &self,
        name: String,
        bind_addr: String,
        bind_port: u16,
        target_host: String,
        target_port: u16,
    ) -> Result<TunnelId, String> {
        let listener = TcpListener::bind((bind_addr.as_str(), bind_port))
            .await
            .map_err(|e| format!("bind {bind_addr}:{bind_port} failed: {e}"))?;
        // The actually-bound port (in case bind_port was 0).
        let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let id = self.alloc_id();
        {
            let mut t = self.tunnels.lock();
            t.insert(
                id,
                TunnelEntry {
                    kind: TunnelKind::Local,
                    name: name.clone(),
                    status: TunnelStatus::Active,
                    bind_addr: bind_addr.clone(),
                    bind_port: local_port,
                    target_host: target_host.clone(),
                    target_port,
                    bytes: 0,
                    accept_task: None,
                    reverse_key: None,
                },
            );
        }
        self.notify();

        let source = self.source.clone();

        let join = crate::backend::TOKIO.spawn(async move {
            loop {
                // Accept next inbound connection. Errors here are transient
                // (e.g. EMFILE); log and continue. A real EOF on the listener
                // (Ok(None)) shouldn't happen for TcpListener, but break to
                // be safe.
                let (tcp, peer) = match listener.accept().await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("SSH: local tunnel accept error: {e}");
                        continue;
                    }
                };
                let peer_addr = peer.ip().to_string();
                let peer_port = peer.port();

                let handle = match source.handle().await {
                    Some(h) => h,
                    None => {
                        tracing::warn!(
                            "SSH: local tunnel inbound conn from {peer_addr} but session is down — dropping"
                        );
                        // Dropping `tcp` closes it.
                        continue;
                    }
                };

                let th = target_host.clone();
                let tp = target_port;

                // Open the direct-tcpip channel and bridge. Each connection
                // gets its own task so the accept loop stays responsive.
                crate::backend::TOKIO.spawn(async move {
                    let channel = {
                        let h = handle.lock().await;
                        match h
                            .channel_open_direct_tcpip(
                                th.clone(),
                                tp as u32,
                                peer_addr.clone(),
                                peer_port as u32,
                            )
                            .await
                        {
                            Ok(ch) => ch,
                            Err(e) => {
                                tracing::warn!(
                                    "SSH: local tunnel channel_open_direct_tcpip to {th}:{tp} failed: {e}"
                                );
                                return;
                            }
                        }
                    };
                    // `bridge` runs until either side EOFs.
                    bridge(channel, tcp).await;
                });
            }
        });

        let abort = join.abort_handle();
        {
            let mut t = self.tunnels.lock();
            if let Some(e) = t.get_mut(&id) {
                e.accept_task = Some(abort);
            }
        }

        Ok(id)
    }

    /// Start a Remote (`-R`) tunnel.
    ///
    /// Asks the SSH server to listen on `bind_addr:bind_port`; inbound
    /// connections there are bridged (via the shared reverse registry + the
    /// `server_channel_open_forwarded_tcpip` handler) back to
    /// `target_host:target_port` on the local side. If `bind_port` is `0`,
    /// the server chooses a port — the chosen port is reflected in the
    /// returned [`TunnelInfo`].
    pub async fn start_remote(
        &self,
        name: String,
        bind_addr: String,
        bind_port: u16,
        target_host: String,
        target_port: u16,
    ) -> Result<TunnelId, String> {
        let handle = self.source.handle().await.ok_or_else(|| {
            "cannot start remote tunnel: SSH session is not connected".to_string()
        })?;

        // Pre-register under the requested port. If the server picks a
        // different port (bind_port == 0), we'll re-register under the
        // returned port below.
        self.source.reverse_registry().insert(
            bind_addr.clone(),
            bind_port as u32,
            LocalTarget {
                host: target_host.clone(),
                port: target_port,
            },
        );

        // `tcpip_forward` is `&mut self` on the russh Handle, so we need the
        // async mutex.
        let bound_port = {
            let mut h = handle.lock().await;
            h.tcpip_forward(bind_addr.clone(), bind_port as u32)
                .await
                .map_err(|e| {
                    // Roll back the registry entry on failure.
                    self.source
                        .reverse_registry()
                        .remove(&bind_addr, bind_port as u32);
                    format!("tcpip_forward({bind_addr}:{bind_port}) failed: {e}")
                })?
        };

        let bound_port = bound_port as u16;

        // If the server chose a port (bind_port == 0), move the registry
        // entry from the requested key to the actual key.
        if bound_port != bind_port {
            let reg = self.source.reverse_registry();
            reg.remove(&bind_addr, bind_port as u32);
            reg.insert(
                bind_addr.clone(),
                bound_port as u32,
                LocalTarget {
                    host: target_host.clone(),
                    port: target_port,
                },
            );
        }

        let id = self.alloc_id();
        {
            let mut t = self.tunnels.lock();
            t.insert(
                id,
                TunnelEntry {
                    kind: TunnelKind::Remote,
                    name,
                    status: TunnelStatus::Active,
                    bind_addr: bind_addr.clone(),
                    bind_port: bound_port,
                    target_host,
                    target_port,
                    bytes: 0,
                    accept_task: None,
                    reverse_key: Some((bind_addr, bound_port as u32)),
                },
            );
        }
        self.notify();

        Ok(id)
    }

    /// Start a Dynamic (`-D`) SOCKS5 tunnel.
    ///
    /// Listens on `bind_addr:bind_port` locally as a SOCKS5 proxy (no-auth,
    /// CONNECT only). For each CONNECT request we open a `direct-tcpip` SSH
    /// channel to the requested target and bridge.
    pub async fn start_dynamic(
        &self,
        name: String,
        bind_addr: String,
        bind_port: u16,
    ) -> Result<TunnelId, String> {
        let listener = TcpListener::bind((bind_addr.as_str(), bind_port))
            .await
            .map_err(|e| format!("bind {bind_addr}:{bind_port} failed: {e}"))?;
        let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let id = self.alloc_id();
        {
            let mut t = self.tunnels.lock();
            t.insert(
                id,
                TunnelEntry {
                    kind: TunnelKind::Dynamic,
                    name,
                    status: TunnelStatus::Active,
                    bind_addr: bind_addr.clone(),
                    bind_port: local_port,
                    target_host: String::new(),
                    target_port: 0,
                    bytes: 0,
                    accept_task: None,
                    reverse_key: None,
                },
            );
        }
        self.notify();

        let source = self.source.clone();

        let join = crate::backend::TOKIO.spawn(async move {
            loop {
                let (tcp, peer) = match listener.accept().await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("SSH: dynamic tunnel accept error: {e}");
                        continue;
                    }
                };
                let peer_addr = peer.ip().to_string();

                let handle = match source.handle().await {
                    Some(h) => h,
                    None => {
                        tracing::warn!(
                            "SSH: dynamic tunnel inbound conn from {peer_addr} but session is down — dropping"
                        );
                        continue;
                    }
                };

                // Each SOCKS connection is handled in its own task: parse the
                // handshake, open the channel, bridge.
                crate::backend::TOKIO.spawn(async move {
                    let (tcp, target_host, target_port) =
                        match socks5_handshake(tcp).await {
                            Ok(t) => t,
                            Err((mut tcp, reason)) => {
                                tracing::debug!(
                                    "SSH: socks5 handshake failed ({reason}) from {peer_addr}"
                                );
                                // Best-effort error reply; handshake helper
                                // already sent the appropriate failure code.
                                let _ = tcp.shutdown().await;
                                return;
                            }
                        };

                    let channel = {
                        let h = handle.lock().await;
                        match h
                            .channel_open_direct_tcpip(
                                target_host.clone(),
                                target_port as u32,
                                peer_addr.clone(),
                                0,
                            )
                            .await
                        {
                            Ok(ch) => ch,
                            Err(e) => {
                                tracing::warn!(
                                    "SSH: dynamic tunnel channel_open_direct_tcpip to {target_host}:{target_port} failed: {e}"
                                );
                                return;
                            }
                        }
                    };
                    bridge(channel, tcp).await;
                });
            }
        });

        let abort = join.abort_handle();
        {
            let mut t = self.tunnels.lock();
            if let Some(e) = t.get_mut(&id) {
                e.accept_task = Some(abort);
            }
        }

        Ok(id)
    }

    /// Stop a single tunnel by id. Safe to call on an already-closed or
    /// unknown id (no-op).
    pub async fn stop(&self, id: TunnelId) {
        let entry = { self.tunnels.lock().remove(&id) };
        let Some(entry) = entry else {
            return;
        };

        match entry.kind {
            TunnelKind::Local | TunnelKind::Dynamic => {
                if let Some(abort) = entry.accept_task {
                    abort.abort();
                }
            }
            TunnelKind::Remote => {
                // Ask the server to stop listening + drop the registry entry.
                if let Some((addr, port)) = entry.reverse_key {
                    self.source.reverse_registry().remove(&addr, port);
                    if let Some(handle) = self.source.handle().await {
                        let h = handle.lock().await;
                        if let Err(e) = h.cancel_tcpip_forward(&addr, port).await {
                            tracing::warn!("SSH: cancel_tcpip_forward({addr}:{port}) failed: {e}");
                        }
                    }
                }
            }
        }

        self.notify();
    }

    /// Stop every tunnel managed by this manager. Used when a tab closes
    /// (borrowed source) to tear down all tunnels started from that panel,
    /// or when an owned session is disconnected.
    pub async fn stop_all(&self) {
        let ids: Vec<TunnelId> = self.tunnels.lock().keys().copied().collect();
        for id in ids {
            self.stop(id).await;
        }
    }

    /// Snapshot of all tunnels (for UI rendering / polling).
    pub fn list(&self) -> Vec<TunnelInfo> {
        let t = self.tunnels.lock();
        let mut out: Vec<TunnelInfo> = t.iter().map(|(id, e)| e.info(*id)).collect();
        out.sort_by_key(|i| i.id);
        out
    }

    /// Snapshot of a single tunnel.
    pub fn get(&self, id: TunnelId) -> Option<TunnelInfo> {
        self.tunnels.lock().get(&id).map(|e| e.info(id))
    }
}

// ---------------------------------------------------------------------------
// SOCKS5 handshake (hand-rolled, no-auth, CONNECT only)
// ---------------------------------------------------------------------------

/// Hand-rolled SOCKS5 negotiation over `tcp`.
///
/// Supports only the no-auth method (0x00) and the CONNECT command (0x01).
/// On success, returns the parsed `(host, port)` target and leaves `tcp`
/// ready for bidirectional bridging. On failure, returns `(tcp, reason)`
/// having already written the appropriate SOCKS5 failure reply so the client
/// sees a clean error rather than a bare close.
///
/// Wire format reference:
/// ```text
/// // Client -> Server: VER NMETHODS METHODS
/// //   VER = 0x05, NMETHODS = 1 byte, METHODS = NMETHODS bytes
/// // Server -> Client: VER METHOD  (we pick 0x00 = no auth)
/// //   0x05 0x00
/// // Client -> Server: VER CMD RSV ATYP DST.ADDR DST.PORT
/// //   VER=0x05, CMD=0x01(CONNECT), RSV=0x00,
/// //   ATYP=0x01(IPv4, 4 bytes)/0x03(domain, 1 len + N bytes)/0x04(IPv6, 16 bytes)
/// // Server -> Client: VER REP RSV ATYP BND.ADDR BND.PORT
/// //   0x05 0x00 0x00 0x01 0.0.0.0 0  (success, no bound addr reported)
/// ```
async fn socks5_handshake(
    mut tcp: tokio::net::TcpStream,
) -> Result<(tokio::net::TcpStream, String, u16), (tokio::net::TcpStream, String)> {
    // --- Method negotiation ---
    // Read VER (0x05) + NMETHODS + METHODS[NMETHODS].
    let mut hdr = [0u8; 2];
    if let Err(e) = tcp.read_exact(&mut hdr).await {
        return Err((tcp, format!("read method header: {e}")));
    }
    if hdr[0] != 0x05 {
        return Err((tcp, format!("bad socks version: {}", hdr[0])));
    }
    let n_methods = hdr[1] as usize;
    if n_methods == 0 {
        // No methods offered — reply "no acceptable methods" (0xFF).
        let _ = tcp.write_all(&[0x05, 0xFF]).await;
        return Err((tcp, "no auth methods offered".into()));
    }
    let mut methods = vec![0u8; n_methods];
    if let Err(e) = tcp.read_exact(&mut methods).await {
        return Err((tcp, format!("read methods: {e}")));
    }
    // We only support no-auth (0x00).
    if !methods.contains(&0x00) {
        let _ = tcp.write_all(&[0x05, 0xFF]).await;
        return Err((
            tcp,
            "no acceptable auth method (only no-auth supported)".into(),
        ));
    }
    // Select no-auth.
    if let Err(e) = tcp.write_all(&[0x05, 0x00]).await {
        return Err((tcp, format!("write method selection: {e}")));
    }

    // --- Request ---
    // Read VER(1) CMD(1) RSV(1) ATYP(1).
    let mut req = [0u8; 4];
    if let Err(e) = tcp.read_exact(&mut req).await {
        return Err((tcp, format!("read request header: {e}")));
    }
    if req[0] != 0x05 {
        return Err((tcp, format!("bad socks version in request: {}", req[0])));
    }
    if req[1] != 0x01 {
        // Only CONNECT is supported. Reply 0x07 (command not supported).
        let _ = tcp
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        return Err((tcp, format!("unsupported socks command: {}", req[1])));
    }
    let atyp = req[3];
    let host = match atyp {
        0x01 => {
            // IPv4: 4 bytes.
            let mut addr = [0u8; 4];
            if let Err(e) = tcp.read_exact(&mut addr).await {
                return Err((tcp, format!("read ipv4 addr: {e}")));
            }
            std::net::Ipv4Addr::from(addr).to_string()
        }
        0x03 => {
            // Domain: 1 length byte + N bytes.
            let mut len_buf = [0u8; 1];
            if let Err(e) = tcp.read_exact(&mut len_buf).await {
                return Err((tcp, format!("read domain len: {e}")));
            }
            let len = len_buf[0] as usize;
            if len == 0 {
                let _ = tcp
                    .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await;
                return Err((tcp, "empty domain".into()));
            }
            let mut buf = vec![0u8; len];
            if let Err(e) = tcp.read_exact(&mut buf).await {
                return Err((tcp, format!("read domain: {e}")));
            }
            match String::from_utf8(buf) {
                Ok(s) => s,
                Err(_) => {
                    let _ = tcp
                        .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    return Err((tcp, "non-utf8 domain".into()));
                }
            }
        }
        0x04 => {
            // IPv6: 16 bytes.
            let mut addr = [0u8; 16];
            if let Err(e) = tcp.read_exact(&mut addr).await {
                return Err((tcp, format!("read ipv6 addr: {e}")));
            }
            std::net::Ipv6Addr::from(addr).to_string()
        }
        other => {
            // Unsupported address type -> 0x08.
            let _ = tcp
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            return Err((tcp, format!("unsupported atyp: {other}")));
        }
    };
    // Port: 2 bytes big-endian.
    let mut port_buf = [0u8; 2];
    if let Err(e) = tcp.read_exact(&mut port_buf).await {
        return Err((tcp, format!("read port: {e}")));
    }
    let port = u16::from_be_bytes(port_buf);

    // --- Success reply ---
    // VER REP RSV ATYP BND.ADDR(4 for IPv4) BND.PORT(2).
    // We report 0.0.0.0:0 — we don't actually expose a bound address since
    // the SSH channel does the real connecting.
    if let Err(e) = tcp
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
    {
        return Err((tcp, format!("write success reply: {e}")));
    }

    Ok((tcp, host, port))
}
