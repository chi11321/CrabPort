//! Port-forwarding tunnel abstraction.
//!
//! Defines the [`CrabPortTunnel`] trait that backends (notably SSH) implement
//! to provide local (forward) and remote (reverse) TCP port forwarding.

use std::net::SocketAddr;

// ---------------------------------------------------------------------------
// Tunnel endpoints
// ---------------------------------------------------------------------------

/// Describes one end of a tunnel — a listen address or a connect target.
#[derive(Debug, Clone)]
pub struct TunnelEndpoint {
    /// IP address or hostname.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl TunnelEndpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

impl From<SocketAddr> for TunnelEndpoint {
    fn from(addr: SocketAddr) -> Self {
        Self {
            host: addr.ip().to_string(),
            port: addr.port(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tunnel status
// ---------------------------------------------------------------------------

/// Current state of a tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    /// The tunnel has been requested but not yet established.
    Pending,
    /// The tunnel is active and forwarding traffic.
    Active,
    /// The tunnel was closed (locally or by the remote).
    Closed,
    /// An error occurred.
    Error,
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// A tunnel capable of setting up **local** (forward) and **remote** (reverse)
/// port forwarding.
///
/// # Local forwarding (`-L`)
///
/// Listens on `listen` on the **client** side and forwards connections to
/// `target` through the tunnel (e.g. through an SSH session).
///
/// # Remote forwarding (`-R`)
///
/// Requests the **remote** (server) side to listen on `listen` and forward
/// connections back to `target` on the client side.
pub trait CrabPortTunnel: Send + Sync {
    // ---- Local (forward) tunnelling ----

    /// Start listening on `listen_addr` and forward all accepted connections
    /// to `target_addr` via the tunnel.
    ///
    /// Returns an opaque tunnel identifier that can be used later to close
    /// this specific tunnel.
    fn local_forward(
        &self,
        listen_addr: TunnelEndpoint,
        target_addr: TunnelEndpoint,
    ) -> anyhow::Result<u64>;

    /// Stop a previously established local forward tunnel.
    fn cancel_local_forward(&self, id: u64) -> anyhow::Result<()>;

    // ---- Remote (reverse) tunnelling ----

    /// Request the remote to listen on `listen_addr` and forward connections
    /// to `target_addr` (on the client side).
    fn remote_forward(
        &self,
        listen_addr: TunnelEndpoint,
        target_addr: TunnelEndpoint,
    ) -> anyhow::Result<u64>;

    /// Stop a previously established remote forward tunnel.
    fn cancel_remote_forward(&self, id: u64) -> anyhow::Result<()>;

    // ---- Status ----

    /// Query the current status of a tunnel by its id.
    fn tunnel_status(&self, id: u64) -> TunnelStatus;

    /// Close all active tunnels and clean up resources.
    fn close_all(&self);
}
