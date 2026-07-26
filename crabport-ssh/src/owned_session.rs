//! [`OwnedSession`] — a standalone SSH connection owned by the tunnel itself.
//!
//! Used by the dedicated Tunnels page where the user wants a connection that
//! exists purely for forwarding (no PTY / shell / SFTP). Unlike the
//! [`crate::SshBackend`] implementation of [`crate::CrabPortTunnel`] (which
//! borrows an already-connected terminal tab's session), an `OwnedSession`
//! establishes and holds its own `russh` `Handle` for the lifetime of the
//! tunnels started on top of it.
//!
//! The connect + authenticate flow mirrors `crate::backend::SshBackend::new`
//! but stops after authentication — no PTY, shell, or SFTP subsystem is
//! opened. The resulting `Handle` is wrapped in `Arc<TokioMutex<...>>` so the
//! `CrabPortTunnel` impl can hand out cheap clones to per-connection tunnel
//! tasks.

use std::sync::Arc;

use parking_lot::RwLock;
use russh::client;
use tokio::sync::Mutex as TokioMutex;

use crabport_terminal::terminal::RemoteStatus;

use crate::TOKIO;
use crate::backend::connect_russh;
use crate::crabport_tunnel::CrabPortTunnel;
use crate::handler::{HostKeyVerifier, SshHandler};
use crate::jump::{JumpChainGuard, connect_through_jumps};
use crate::keys::decode_private_key;
use crate::known_hosts::KnownHosts;
use crate::session::SshConnectionInfo;
use ::crabport_tunnel::ReverseForwardRegistry;

/// A standalone SSH connection owned by the tunnel layer.
///
/// Construct via [`OwnedSession::connect`], then wrap in `Arc<dyn
/// CrabPortTunnel>` (or use directly via the `CrabPortTunnel` impl) to feed
/// into a [`crate::TunnelManager`].
pub struct OwnedSession {
    /// The authenticated `russh` handle, wrapped for shared access by tunnel
    /// per-connection tasks. `None` only transiently during connect, and
    /// permanently after disconnect.
    handle: Arc<TokioMutex<Option<Arc<TokioMutex<client::Handle<SshHandler>>>>>>,
    /// Connection status (mirrors `SshBackend::monitor`'s status field).
    status: Arc<RwLock<RemoteStatus>>,
    /// Reverse-forward registry shared with the `SshHandler`.
    reverse_registry: ReverseForwardRegistry,
}

impl OwnedSession {
    /// Connect and authenticate a standalone SSH session for tunneling.
    ///
    /// Reuses the connect (`crate::backend::connect_russh`) + authenticate
    /// (public key if `info.private_key` is set, else password) flow from
    /// `SshBackend::new`, but does NOT open a PTY / shell / SFP — the session
    /// exists purely to carry `direct-tcpip` / `tcpip_forward` channels.
    ///
    /// `verifier` is the host-key prompt callback; `None` means auto-reject
    /// unknown hosts (same semantics as `SshBackend::new`).
    pub async fn connect(
        info: SshConnectionInfo,
        verifier: Option<HostKeyVerifier>,
    ) -> Result<Arc<Self>, String> {
        let handle: Arc<TokioMutex<Option<Arc<TokioMutex<client::Handle<SshHandler>>>>>> =
            Arc::new(TokioMutex::new(None));
        let status: Arc<RwLock<RemoteStatus>> = Arc::new(RwLock::new(RemoteStatus::Connecting));
        let reverse_registry = ReverseForwardRegistry::new();
        let reverse_registry_for_handler = reverse_registry.clone();

        let handle_ret = handle.clone();
        let status_ret = status.clone();
        let host_for_handler = info.host.clone();
        let port_for_handler = info.port;
        let verifier_for_handler = verifier.clone();

        // Run the connect + auth on the shared SSH tokio runtime (russh
        // requires tokio), surfacing the outcome via a oneshot so `connect`
        // returns cleanly once authentication finishes (or fails).
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        TOKIO.spawn(async move {
            // Open the known_hosts store (non-fatal on failure — fall back to
            // prompting, same as SshBackend).
            let known_hosts = match KnownHosts::open() {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("SSH: could not open known_hosts store ({e})");
                    None
                }
            };

            let handler = SshHandler {
                host: host_for_handler,
                port: port_for_handler,
                known_hosts,
                verifier: verifier_for_handler.clone(),
                reverse_registry: reverse_registry_for_handler,
            };

            let config = Arc::new(client::Config::default());
            // Direct or through the jump-host chain (mirrors
            // `SshBackend::new`). The returned guard keeps the intermediate
            // hop sessions alive; it's parked on this task below.
            let connect_result = if info.jump_hosts.is_empty() {
                connect_russh(config, &info.proxy, &info.host, info.port, handler)
                    .await
                    .map(|sh| (sh, JumpChainGuard::empty()))
                    .map_err(|e| e.to_string())
            } else {
                connect_through_jumps(
                    config,
                    &info.jump_hosts,
                    &info.host,
                    info.port,
                    handler,
                    verifier_for_handler,
                    &|_msg| {},
                )
                .await
            };
            let (mut sh, jump_guard) = match connect_result {
                Ok(ok) => ok,
                Err(e) => {
                    tracing::error!("SSH: owned session connect failed: {e}");
                    *status.write() = RemoteStatus::Disconnected;
                    let _ = tx.send(Err(format!("connect failed: {e}")));
                    return;
                }
            };

            // Authenticate — key auth if a private key is set, else password.
            if info.uses_key_auth() {
                let key_str = info.private_key.as_deref().unwrap_or("");
                let key_pair = match decode_private_key(key_str, info.passphrase.as_deref()) {
                    Ok(kp) => kp,
                    Err(e) => {
                        tracing::error!("SSH: owned session — failed to decode private key: {e}");
                        *status.write() = RemoteStatus::Disconnected;
                        let _ = tx.send(Err(format!("public key decode failed: {e}")));
                        return;
                    }
                };
                match sh
                    .authenticate_publickey(&info.username, Arc::new(key_pair))
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        *status.write() = RemoteStatus::Disconnected;
                        let _ = tx.send(Err("public key authentication failed".into()));
                        return;
                    }
                    Err(e) => {
                        tracing::error!("SSH: owned session key auth failed: {e}");
                        *status.write() = RemoteStatus::Disconnected;
                        let _ = tx.send(Err(format!("public key authentication failed: {e}")));
                        return;
                    }
                }
            } else {
                match sh
                    .authenticate_password(&info.username, &info.password)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        *status.write() = RemoteStatus::Disconnected;
                        let _ = tx.send(Err("password authentication failed".into()));
                        return;
                    }
                    Err(e) => {
                        tracing::error!("SSH: owned session password auth failed: {e}");
                        *status.write() = RemoteStatus::Disconnected;
                        let _ = tx.send(Err(format!("password authentication failed: {e}")));
                        return;
                    }
                }
            }

            // Authentication succeeded — publish the handle and mark connected.
            let shared = Arc::new(TokioMutex::new(sh));
            *handle.lock().await = Some(shared.clone());
            *status.write() = RemoteStatus::Connected;
            let _ = tx.send(Ok(()));

            // Note: unlike SshBackend we do NOT spawn a monitor loop (the
            // Tunnels page doesn't display remote perf metrics) and we do NOT
            // drive an event loop here — russh's `Handle` runs its own
            // background task. The Handle (and its connection) lives as long
            // as the `Arc<TokioMutex<Handle>>` is held: by this `OwnedSession`
            // and by any in-flight tunnel per-connection tasks. When the last
            // `Arc` clone drops, russh closes the connection.
            //
            // Keep a long-lived reference on this spawned task so the
            // connection isn't torn down the moment `connect()` returns: the
            // `shared` clone above is dropped at end of scope, but the one
            // stored under `handle` keeps it alive. The jump-chain guard is
            // parked here too — dropping it would tear down the bastion
            // sessions the target connection rides on. Block this task
            // forever so the runtime doesn't drop the worker; it's cheap
            // (no polling).
            let _jump_guard = jump_guard;
            std::future::pending::<()>().await;
        });

        match rx.await {
            Ok(Ok(())) => Ok(Arc::new(Self {
                handle: handle_ret,
                status: status_ret,
                reverse_registry,
            })),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("owned session connect task panicked".into()),
        }
    }
}

#[async_trait::async_trait]
impl CrabPortTunnel for OwnedSession {
    async fn handle(&self) -> Option<Arc<TokioMutex<client::Handle<SshHandler>>>> {
        self.handle.lock().await.clone()
    }

    fn status(&self) -> RemoteStatus {
        *self.status.read()
    }

    fn reverse_registry(&self) -> Arc<ReverseForwardRegistry> {
        // `ReverseForwardRegistry` is `Default + Clone` over an inner `Arc`,
        // so cloning produces a cheap shared handle to the same map.
        Arc::new(self.reverse_registry.clone())
    }
}
