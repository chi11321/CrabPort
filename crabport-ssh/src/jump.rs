//! Jump-host (bastion) chain establishment — the SSH-level analogue of
//! OpenSSH's `ProxyJump`.
//!
//! The chain is built hop by hop: the first hop is reached over raw TCP
//! (optionally through that hop's proxy), each subsequent hop — and finally
//! the target — is reached by opening a `direct-tcpip` channel on the
//! previous hop's authenticated session and running the next SSH handshake
//! over that channel's stream (`Channel::into_stream()`).
//!
//! Every hop gets its own [`SshHandler`], so host-key verification (TOFU via
//! `known_hosts` + UI prompt) applies to jump hosts exactly like it does to
//! direct connections.
//!
//! The intermediate sessions must stay alive for as long as the target
//! session exists — dropping a hop's `Handle` tears down its session and
//! with it the tunnelled stream of every hop after it. Callers therefore
//! hold the returned [`JumpChainGuard`] alongside the target handle (the
//! terminal backend moves it into its event-loop task; the owned tunnel
//! session parks it on its keep-alive task).

use std::sync::Arc;

use russh::client;

use crate::backend::connect_russh;
use crate::handler::{HostKeyVerifier, SshHandler};
use crate::keys::decode_private_key;
use crate::known_hosts::KnownHosts;
use crate::session::JumpHostInfo;
use ::crabport_tunnel::ReverseForwardRegistry;
use secrecy::ExposeSecret;

/// Keeps the intermediate jump-host sessions alive for the lifetime of the
/// target connection. Dropping the guard drops the hop `Handle`s, letting
/// russh tear the jump sessions down.
pub(crate) struct JumpChainGuard {
    #[allow(dead_code)]
    handles: Vec<client::Handle<SshHandler>>,
}

impl JumpChainGuard {
    /// A guard for a direct (no-jump) connection.
    pub(crate) fn empty() -> Self {
        Self {
            handles: Vec::new(),
        }
    }
}

/// Build the per-hop [`SshHandler`]. Each hop verifies its own host key
/// against `known_hosts` (opened fresh per hop — cheap, and a failure only
/// means that hop prompts) and shares the caller's UI verifier. The
/// reverse-forward registry is a fresh dummy: `-R` tunnels only ever target
/// the final host, whose handler is supplied by the caller.
fn hop_handler(host: &str, port: u16, verifier: Option<HostKeyVerifier>) -> SshHandler {
    let known_hosts = match KnownHosts::open() {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("SSH: could not open known_hosts store for jump hop ({e})");
            None
        }
    };
    SshHandler {
        host: host.to_string(),
        port,
        known_hosts,
        verifier,
        reverse_registry: ReverseForwardRegistry::new(),
    }
}

/// Authenticate one jump hop with its own credentials (key auth when a
/// private key is set, password otherwise — mirrors the target auth flow).
async fn authenticate_hop(
    sh: &mut client::Handle<SshHandler>,
    hop: &JumpHostInfo,
) -> Result<(), String> {
    let label = format!("{}@{}:{}", hop.username, hop.host, hop.port);
    if let Some(key) = hop
        .private_key
        .as_ref()
        .map(|s| s.expose_secret())
        .filter(|k| !k.is_empty())
    {
        let key_pair = decode_private_key(key, hop.passphrase.as_ref().map(|s| s.expose_secret()))
            .map_err(|e| format!("jump host {label}: private key decode failed: {e}"))?;
        match sh
            .authenticate_publickey(&hop.username, Arc::new(key_pair))
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "jump host {label}: public key authentication failed"
            )),
            Err(e) => Err(format!(
                "jump host {label}: public key authentication failed: {e}"
            )),
        }
    } else {
        match sh
            .authenticate_password(&hop.username, hop.password.expose_secret())
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("jump host {label}: password authentication failed")),
            Err(e) => Err(format!(
                "jump host {label}: password authentication failed: {e}"
            )),
        }
    }
}

/// Open a `direct-tcpip` channel on `sh` to `host:port` and return it as a
/// tokio stream ready to carry the next SSH handshake.
async fn open_tunnel_stream(
    sh: &client::Handle<SshHandler>,
    host: &str,
    port: u16,
) -> Result<russh::ChannelStream<client::Msg>, String> {
    let channel = sh
        .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| format!("direct-tcpip to {host}:{port} failed: {e}"))?;
    Ok(channel.into_stream())
}

/// Connect to `target_host:target_port` through the `jump_hosts` chain and
/// run the target's SSH handshake with `target_handler`.
///
/// Returns the target's connected (NOT yet authenticated) `Handle` plus the
/// [`JumpChainGuard`] holding every intermediate session alive. The caller
/// authenticates the target itself, keeping its existing status/error
/// reporting flow.
///
/// `jump_hosts` must be non-empty — direct connections keep using
/// [`connect_russh`].
pub(crate) async fn connect_through_jumps(
    config: Arc<client::Config>,
    jump_hosts: &[JumpHostInfo],
    target_host: &str,
    target_port: u16,
    target_handler: SshHandler,
    verifier: Option<HostKeyVerifier>,
    on_status: &(dyn Fn(String) + Send + Sync),
) -> Result<(client::Handle<SshHandler>, JumpChainGuard), String> {
    let mut handles: Vec<client::Handle<SshHandler>> = Vec::with_capacity(jump_hosts.len());

    for (i, hop) in jump_hosts.iter().enumerate() {
        let label = format!("{}@{}:{}", hop.username, hop.host, hop.port);
        on_status(format!(
            "Connecting to jump host {}/{} ({})...",
            i + 1,
            jump_hosts.len(),
            label
        ));

        let handler = hop_handler(&hop.host, hop.port, verifier.clone());
        let mut sh = match handles.last() {
            // First hop: raw TCP, optionally through the hop's proxy.
            None => connect_russh(config.clone(), &hop.proxy, &hop.host, hop.port, handler)
                .await
                .map_err(|e| format!("jump host {label}: connect failed: {e}"))?,
            // Later hops: tunnel through the previous hop.
            Some(prev) => {
                let stream = open_tunnel_stream(prev, &hop.host, hop.port).await?;
                client::connect_stream(config.clone(), stream, handler)
                    .await
                    .map_err(|e| format!("jump host {label}: handshake failed: {e}"))?
            }
        };

        on_status(format!("Authenticating with jump host {label}..."));
        authenticate_hop(&mut sh, hop).await?;
        on_status(format!("Jump host {label} ready"));
        handles.push(sh);
    }

    // Final hop: tunnel from the last jump host to the actual target and
    // run the target's handshake over it.
    on_status(format!(
        "Opening tunnel to {}:{} via jump host...",
        target_host, target_port
    ));
    let last = handles.last().expect("jump_hosts must be non-empty");
    let stream = open_tunnel_stream(last, target_host, target_port).await?;
    let target = client::connect_stream(config, stream, target_handler)
        .await
        .map_err(|e| format!("target handshake via jump host failed: {e}"))?;

    Ok((target, JumpChainGuard { handles }))
}
