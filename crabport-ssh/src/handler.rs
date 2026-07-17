use std::pin::Pin;
use std::sync::Arc;

use russh::client::{self, Msg};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::known_hosts::{KnownHost, KnownHosts, LookupResult};
use ::crabport_tunnel::ReverseForwardRegistry;

// ---------------------------------------------------------------------------
// Host-key verification (TOFU)
// ---------------------------------------------------------------------------

/// Information about a server's presented host key, passed to the UI so the
/// user can decide whether to trust an unknown host.
#[derive(Debug, Clone)]
pub struct HostKeyInfo {
    /// Remote hostname / IP as supplied by the caller.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// Key algorithm name, e.g. `ssh-ed25519`.
    pub algo: String,
    /// SHA-256 base64 (nopad) fingerprint of the key.
    pub fingerprint: String,
}

/// A boxed future returned by [`HostKeyVerifier`].
pub type HostKeyVerifyFuture = Pin<Box<dyn std::future::Future<Output = bool> + Send>>;

/// Callback used to ask the UI whether to trust an unknown host key.
///
/// The future resolves to `true` when the user accepts the key (the caller
/// will then persist it to `known_hosts` and continue the connection), or
/// `false` when the user declines (the connection is aborted).
pub type HostKeyVerifier = Arc<dyn Fn(HostKeyInfo) -> HostKeyVerifyFuture + Send + Sync>;

// ---------------------------------------------------------------------------
// SSH client handler
// ---------------------------------------------------------------------------

pub struct SshHandler {
    /// Connection target — used for `known_hosts` lookup / persistence.
    pub(crate) host: String,
    pub(crate) port: u16,
    /// Persistent TOFU store. Opened lazily on the connecting task so a
    /// missing store never blocks a connection attempt — the worst case
    /// is that every connect prompts the user.
    pub(crate) known_hosts: Option<KnownHosts>,
    /// UI prompt callback for unknown hosts. `None` means "auto-reject"
    /// (no way to confirm), which is safer than auto-accept.
    pub(crate) verifier: Option<HostKeyVerifier>,
    /// Registry of active Remote (`-R`) forwards. When the server reports a
    /// new inbound connection on a forwarded port, the
    /// `server_channel_open_forwarded_tcpip` callback consults this map to
    /// find the local `host:port` the connection should be bridged to.
    /// Shared (via `Clone`) between the handler and the `TunnelManager` that
    /// registered the forward.
    pub(crate) reverse_registry: ReverseForwardRegistry,
}

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let algo = server_public_key.name().to_string();
        let fingerprint = server_public_key.fingerprint();

        // 1. Consult known_hosts (TOFU).
        if let Some(store) = &self.known_hosts {
            match store.lookup(&self.host, self.port, &algo, &fingerprint) {
                Ok(LookupResult::Matched) => {
                    tracing::debug!(
                        "SSH: known_hosts match for {}:{} ({} {})",
                        self.host,
                        self.port,
                        algo,
                        fingerprint
                    );
                    return Ok(true);
                }
                Ok(LookupResult::Mismatched {
                    expected_algo,
                    expected_fingerprint,
                }) => {
                    tracing::error!(
                        "SSH: host key mismatch for {}:{} — expected {} {}, got {} {}",
                        self.host,
                        self.port,
                        expected_algo,
                        expected_fingerprint,
                        algo,
                        fingerprint
                    );
                    // Mismatch is a hard failure — do not prompt.
                    return Ok(false);
                }
                Ok(LookupResult::NotFound) => {
                    // Fall through to user prompt below.
                }
                Err(e) => {
                    tracing::warn!(
                        "SSH: known_hosts lookup failed for {}:{} ({e}); falling back to prompt",
                        self.host,
                        self.port
                    );
                }
            }
        }

        // 2. Unknown host — ask the user via the UI verifier.
        let Some(verifier) = self.verifier.clone() else {
            tracing::error!(
                "SSH: unknown host {}:{} and no verifier wired — refusing to connect",
                self.host,
                self.port
            );
            return Ok(false);
        };

        let info = HostKeyInfo {
            host: self.host.clone(),
            port: self.port,
            algo: algo.clone(),
            fingerprint: fingerprint.clone(),
        };
        let accepted = verifier(info).await;

        if accepted {
            // Persist the new entry so future connections don't prompt again.
            if let Some(store) = &self.known_hosts {
                let entry = KnownHost {
                    host: self.host.clone(),
                    port: self.port,
                    algo: algo.clone(),
                    fingerprint: fingerprint.clone(),
                };
                if let Err(e) = store.add(&entry) {
                    tracing::warn!(
                        "SSH: failed to persist known_hosts entry for {}:{} ({e})",
                        self.host,
                        self.port
                    );
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Called by the server when a connection arrives on a port the client
    /// asked it to listen on via `tcpip_forward` (a Remote / `-R` tunnel).
    ///
    /// We look up `(connected_address, connected_port)` in the reverse
    /// registry to find the local `host:port` the inbound connection should
    /// be bridged to, then spawn a bidirectional copy between the SSH channel
    /// and a local `TcpStream`. If no forward is registered for this address
    /// (e.g. it was just removed), we simply drop the channel — the server
    /// will close it.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let target = match self
            .reverse_registry
            .lookup(connected_address, connected_port)
        {
            Some(t) => t,
            None => {
                tracing::warn!(
                    "SSH: reverse forward hit with no registered target for {}:{} \
                     (originator {}:{}) — dropping channel",
                    connected_address,
                    connected_port,
                    originator_address,
                    originator_port
                );
                // Dropping `channel` closes it.
                return Ok(());
            }
        };

        tracing::debug!(
            "SSH: reverse forward {}:{} -> {}:{} (originator {}:{})",
            connected_address,
            connected_port,
            target.host,
            target.port,
            originator_address,
            originator_port
        );

        let target_host = target.host.clone();
        let target_port = target.port;

        // Spawn the bridge on the shared SSH tokio runtime. The handler must
        // return promptly so the russh event loop keeps draining other
        // channels.
        crate::backend::TOKIO.spawn(async move {
            let tcp = match TcpStream::connect((target_host.as_str(), target_port)).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "SSH: reverse forward bridge — connect to {}:{} failed ({e})",
                        target_host,
                        target_port
                    );
                    return;
                }
            };

            bridge(channel, tcp).await;
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bidirectional bridge between an SSH channel and a TCP stream
// ---------------------------------------------------------------------------
//
// `Channel<Msg>` does not itself implement `AsyncRead + AsyncWrite`, but
// `Channel::into_stream()` returns a `ChannelStream` that does. `ChannelStream`
// is `!Unpin`, but `tokio::io::split` (unlike `tokio::io::copy`) requires only
// `AsyncRead + AsyncWrite` (not `Unpin`) and produces `ReadHalf`/`WriteHalf`
// that ARE `Unpin` (they wrap an `Arc`), so `tokio::io::copy` is then happy to
// consume them. This is the cleanest way to bridge a russh channel to a
// `TcpStream` bidirectionally.
//
// The bridge runs until EITHER direction completes (EOF or error), then
// returns — the dropped halves close the underlying streams.

/// Bridge an SSH channel and a TCP stream bidirectionally, copying data in
/// both directions until either side EOFs or errors.
///
/// Used by both the reverse-forward handler (Remote `-R` tunnels, here in
/// `handler.rs`) and the Local/Dynamic accept loops in `tunnels.rs`.
pub(crate) async fn bridge(channel: russh::Channel<Msg>, tcp: TcpStream) {
    let stream = channel.into_stream();
    let (mut ch_rx, mut ch_tx) = tokio::io::split(stream);
    let (mut tcp_rx, mut tcp_tx) = tokio::io::split(tcp);

    // Run both copies concurrently; return as soon as either completes.
    // Errors are logged at debug level and swallowed — the bridge is best
    // effort and the caller already owns the user-visible tunnel state.
    tokio::join!(
        async {
            if let Err(e) = tokio::io::copy(&mut ch_rx, &mut tcp_tx).await {
                tracing::debug!("SSH: bridge channel->tcp copy ended with error: {e}");
            }
            // Signal EOF on the tcp write side so the peer sees a clean close.
            let _ = tcp_tx.shutdown().await;
        },
        async {
            if let Err(e) = tokio::io::copy(&mut tcp_rx, &mut ch_tx).await {
                tracing::debug!("SSH: bridge tcp->channel copy ended with error: {e}");
            }
            // Signal EOF on the channel write side.
            let _ = ch_tx.shutdown().await;
        }
    );
}
