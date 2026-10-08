//! What the agent's tools need from a terminal session, and what the UI side
//! provides for it.
//!
//! The trait is deliberately context-free — no GPUI app handle, no entity
//! reads. A UI implements it as a cheap snapshot taken when a tool call is
//! approved: the shared session/backend handles, the directory the shell last
//! reported, an event subscription for transfer completions, the
//! Tunnels-page registry, and a channel that routes registry mutations back to
//! the UI thread (where the app's own tunnel machinery lives). That is what
//! lets [`crate::Agent`] run its work — including the polling loops — on
//! background threads, with the UI only awaiting the result.

use std::sync::Arc;
use std::time::Duration;

use async_broadcast::Receiver;
use crabport_sftp::FileEntry;
use crabport_ssh::{TunnelKind, TunnelManager};
use crabport_terminal::terminal::{BackendEvent, ExecCallback, ExecCancel, ExecOutput};

/// Snapshot of one Tunnels-page config, scoped by the UI to the session's
/// host. The agent reports these and may start/stop/delete them; it never
/// sees another host's (let alone another terminal's) tunnels.
#[derive(Clone)]
pub struct RegistryTunnel {
    pub id: i64,
    pub name: String,
    pub kind: TunnelKind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub running: bool,
    /// `true` when the run borrows a terminal's connection (as opposed to a
    /// dedicated one started from the Tunnels page).
    pub borrowed: bool,
}

/// A terminal session as the agent's tools see it.
pub trait AgentSession: Send + Sync {
    /// Whether out-of-band commands can run here (`terminal_exec`).
    fn allow_exec_capture(&self) -> bool {
        false
    }

    /// Whether this session supports SFTP (`sftp_*`).
    fn allow_sftp(&self) -> bool {
        false
    }

    /// Whether this session can host tunnels — an SSH connection is required.
    fn allow_tunnels(&self) -> bool {
        false
    }

    /// The directory the shell last reported, when shell integration says.
    fn cwd(&self) -> Option<String> {
        None
    }

    /// The host this session is connected to, when it came from a saved host.
    fn host_id(&self) -> Option<i64> {
        None
    }

    /// The most recent terminal output lines (scrollback + visible screen).
    fn dump_text(&self, _lines: usize) -> String {
        String::new()
    }

    /// Non-blocking [`Self::dump_text`]: `None` while the terminal reader
    /// holds its lock — the caller retries on its next tick.
    fn try_dump_text(&self, _lines: usize) -> Option<String> {
        None
    }

    /// Type raw bytes into the interactive shell — the visible run's input.
    fn write_raw(&self, _data: &[u8]) {}

    /// Start an out-of-band command. `done` runs exactly once, on a
    /// backend-owned thread; `cancel` stops the command when the user asks.
    fn exec_capture(
        &self,
        _command: &str,
        _timeout: Duration,
        _cancel: ExecCancel,
        done: ExecCallback,
    ) {
        done(ExecOutput {
            output: "captured execution is not supported on this connection".to_string(),
            ..Default::default()
        });
    }

    /// The backend's event stream. The SFTP transfer tools subscribe *before*
    /// starting a transfer: its `SftpTransferFinished` event is the only
    /// completion signal there is.
    fn subscribe_events(&self) -> Option<Receiver<BackendEvent>> {
        None
    }

    /// The resolved remote directory and its listing, once a navigation has
    /// landed. The `Arc` identity is the freshness signal: the backend
    /// installs a new one on every successful read_dir.
    fn sftp_listing(&self) -> Option<(String, Arc<Vec<FileEntry>>)> {
        None
    }

    /// Ask the backend to list `path`; the result lands in
    /// [`Self::sftp_listing`].
    fn sftp_navigate(&self, _path: &str) {}

    /// Start a download / upload. Completion shows up as a
    /// [`BackendEvent::SftpTransferFinished`] on [`Self::subscribe_events`].
    fn sftp_download(&self, _remote_path: &str, _local_path: &str) {}
    fn sftp_upload(&self, _local_path: &str, _remote_path: &str) {}

    /// This session's tunnel manager — one per session, created by the UI so
    /// the agent's tunnels ride this connection and no other.
    fn tunnel_manager(&self) -> Option<Arc<TunnelManager>> {
        None
    }

    /// Tunnels configured for this session's host (running or stopped).
    fn registry_tunnels(&self) -> Vec<RegistryTunnel> {
        Vec::new()
    }

    fn registry_is_running(&self, _config_id: i64) -> bool {
        false
    }

    /// Live addresses of a running registry tunnel:
    /// `(bind_addr, bind_port, target_host, target_port)`.
    fn registry_live(&self, _config_id: i64) -> Option<(String, u16, String, u16)> {
        None
    }

    /// Ask the UI to start a registry tunnel borrowing this session, and to
    /// stop / delete one. The UI routes these through the app's own tunnel
    /// machinery, so the Tunnels page and the store stay consistent.
    fn registry_open(&self, _config_id: i64) {}
    fn registry_close(&self, _config_id: i64) {}
    fn registry_delete(&self, _config_id: i64) {}
}
