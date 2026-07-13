use std::sync::Arc;

use async_broadcast::Receiver as BroadcastReceiver;

use crabport_sftp::CrabPortSftp;
use crabport_terminal::terminal::{
    BackendEvent, CrabPortMonitor, CrabPortTerminal, RemoteMetrics, RemoteStatus, SftpTransferKind,
};

use crate::backend::{Command, SshBackend, TOKIO};
use crate::transfer::SftpTransferHandle;

impl CrabPortTerminal for SshBackend {
    fn write(&self, data: &[u8]) {
        let _ = self.command_tx.try_send(Command::Write(data.to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.command_tx.try_send(Command::Resize(cols, rows));
    }

    fn close(&self) {
        let _ = self.command_tx.try_send(Command::Close);
    }

    fn subscribe(&self) -> BroadcastReceiver<BackendEvent> {
        self.event_tx.new_receiver()
    }

    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        Some(self)
    }

    fn allow_sftp(&self) -> bool {
        true
    }

    fn allow_tunnels(&self) -> bool {
        true
    }

    fn refresh_history(&self) {
        let handle = self.handle.clone();
        let event_tx = self.event_tx.clone();
        TOKIO.spawn(async move {
            // Acquire the shared SSH handle. If the session isn't ready yet
            // (still connecting), bail silently — the caller can refresh
            // again once the connection is up.
            let hg = handle.lock().await;
            let Some(h) = hg.as_ref() else {
                return;
            };
            let h = h.lock().await;

            // Pick the most-likely shell history file. We try zsh first
            // (extended format) then bash (plain lines) then a generic
            // `~/.history`. The remote shell writes to its own file as
            // commands run, so this is reasonably live.
            //
            // We keep the command small and POSIX-portable: `for f in ...`
            // loops aren't universally available in non-interactive sh,
            // so we use explicit `[ -r ] && cat` fallbacks.
            let cmd = "f=$HOME/.zsh_history; [ -r \"$f\" ] || f=$HOME/.bash_history; \
                 [ -r \"$f\" ] || f=$HOME/.history; \
                 [ -r \"$f\" ] && cat \"$f\"";
            let Some(raw) = crate::monitor::exec_and_read(&h, cmd).await else {
                return;
            };
            let cmds = parse_shell_history(&raw);
            let _ = event_tx.broadcast(BackendEvent::HistoryLoaded(cmds)).await;
        });
    }

    fn sftp_entries(&self) -> Option<Arc<Vec<crabport_sftp::FileEntry>>> {
        self.sftp_entries.read().clone()
    }

    fn sftp_cwd(&self) -> Option<Arc<String>> {
        self.sftp_cwd.read().clone()
    }

    fn sftp_navigate(&self, path: &str) {
        let handle = self.handle.clone();
        let entries = self.sftp_entries.clone();
        let cwd = self.sftp_cwd.clone();
        let sftp_session = self.sftp_session.clone();
        let path = path.to_string();
        TOKIO.spawn(async move {
            // Reuse the cached SFTP session if we still have one. Only
            // (re)connect when the cache is empty — e.g. on the very first
            // navigate after a connect that didn't establish SFTP, or after
            // the session was dropped following an error. This avoids paying
            // the ~24ms SFTP handshake on every directory change.
            let sftp = {
                let mut guard = sftp_session.lock().await;
                if guard.is_none() {
                    let hg = handle.lock().await;
                    let Some(h) = hg.as_ref() else {
                        return;
                    };
                    let h = h.lock().await;
                    match crabport_sftp::SftpBackend::connect(&*h).await {
                        Ok(s) => *guard = Some(s),
                        Err(e) => {
                            tracing::warn!("SFTP navigate: connect failed ({e})");
                            return;
                        }
                    }
                }
                // Take the session out of the cache for the duration of this
                // operation so concurrent navigations don't fight over the
                // same channel. We put it back (or drop it on error) below.
                guard.take().expect("just ensured Some")
            };

            // Resolve the target path
            let resolved = match sftp.canonicalize(&path).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("SFTP navigate: canonicalize '{}' failed ({e})", path);
                    // Drop the session — the channel may be dead.
                    let _ = sftp.close().await;
                    return;
                }
            };
            match sftp.read_dir(&resolved).await {
                Ok(dir_entries) => {
                    *entries.write() = Some(Arc::new(dir_entries));
                    *cwd.write() = Some(Arc::new(resolved));
                    // Return the live session to the cache.
                    *sftp_session.lock().await = Some(sftp);
                }
                Err(e) => {
                    tracing::warn!("SFTP navigate: read_dir failed ({e})");
                    let _ = sftp.close().await;
                }
            }
        });
    }

    fn sftp_download(&self, remote_path: &str, local_path: &str) {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let remote_path = remote_path.to_string();
        let local_path = local_path.to_string();
        self.spawn_transfer(async move {
            let result =
                crate::transfer::sftp_download_impl(&backend, &remote_path, &local_path).await;
            let (success, message) = match &result {
                Ok(()) => (true, format!("downloaded {local_path}")),
                Err(e) => (false, format!("download failed: {e}")),
            };
            let _ = event_tx
                .broadcast(BackendEvent::SftpTransferFinished {
                    kind: SftpTransferKind::Download,
                    success,
                    message,
                })
                .await;
        });
    }

    fn sftp_upload(&self, local_path: &str, remote_path: &str) {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let remote_path = remote_path.to_string();
        let local_path = local_path.to_string();
        self.spawn_transfer(async move {
            let result =
                crate::transfer::sftp_upload_impl(&backend, &local_path, &remote_path).await;
            let (success, message) = match result {
                Ok(()) => (true, format!("uploaded {remote_path}")),
                Err(e) => (false, format!("upload failed: {e}")),
            };
            let _ = event_tx
                .broadcast(BackendEvent::SftpTransferFinished {
                    kind: SftpTransferKind::Upload,
                    success,
                    message,
                })
                .await;
        });
    }

    fn sftp_upload_batch(&self, items: &[(String, String)]) {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let items = items.to_vec();
        self.spawn_transfer(async move {
            let result = crate::transfer::sftp_upload_batch_impl(&backend, &items).await;
            let (success, message) = match result {
                Ok(()) => (true, format!("uploaded {} files", items.len())),
                Err(e) => (false, format!("batch upload failed: {e}")),
            };
            let _ = event_tx
                .broadcast(BackendEvent::SftpTransferFinished {
                    kind: SftpTransferKind::Upload,
                    success,
                    message,
                })
                .await;
        });
    }

    fn sftp_delete(&self, remote_path: &str) {
        // Reuse the SftpTransferHandle so we get the cached session + event
        // sink. There's no actual transfer, but we emit a `SftpTransferFinished`
        // so the existing UI finish handling (toolbar clear, overlay log)
        // applies. We use the Download kind arbitrarily — the message text
        // carries the real semantics.
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let remote_path = remote_path.to_string();
        self.spawn_transfer(async move {
            let result = crate::transfer::sftp_delete_impl(&backend, &remote_path).await;
            let (success, message) = match result {
                Ok(()) => (true, format!("deleted {remote_path}")),
                Err(e) => (false, format!("delete failed: {e}")),
            };
            let _ = event_tx
                .broadcast(BackendEvent::SftpTransferFinished {
                    kind: SftpTransferKind::Delete,
                    success,
                    message,
                })
                .await;
        });
    }

    fn sftp_rename(&self, old_path: &str, new_path: &str) {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let old_path = old_path.to_string();
        let new_path = new_path.to_string();
        self.spawn_transfer(async move {
            let result = crate::transfer::sftp_rename_impl(&backend, &old_path, &new_path).await;
            let (success, message) = match result {
                Ok(()) => (true, format!("renamed {old_path} → {new_path}")),
                Err(e) => (false, format!("rename failed: {e}")),
            };
            let _ = event_tx
                .broadcast(BackendEvent::SftpTransferFinished {
                    kind: SftpTransferKind::Rename,
                    success,
                    message,
                })
                .await;
        });
    }

    fn sftp_open_in_editor(&self, remote_path: &str) {
        let backend = SftpTransferHandle {
            handle: self.handle.clone(),
            sftp_session: self.sftp_session.clone(),
            event_tx: Some(self.event_tx.clone()),
        };
        let event_tx = self.event_tx.clone();
        let remote_path = remote_path.to_string();
        self.spawn_transfer(async move {
            let result = crate::transfer::sftp_edit_impl(&backend, &remote_path).await;
            // Only emit a Finished event on failure — success means the edit
            // session ended normally (user closed the editor or the 30-min
            // bound elapsed), which is silent per the product spec.
            if let Err(e) = result {
                let _ = event_tx
                    .broadcast(BackendEvent::SftpTransferFinished {
                        kind: SftpTransferKind::Edit,
                        success: false,
                        message: format!("edit failed: {e}"),
                    })
                    .await;
            }
        });
    }

    fn spawn_channel(
        &self,
        cols: u16,
        rows: u16,
    ) -> Option<std::sync::Arc<dyn crabport_terminal::terminal::CrabPortTerminal>> {
        // `new_channel_backend` is async (it awaits the SSH handle lock +
        // channel allocation on the tokio runtime). We can't await here, so
        // spawn it on TOKIO and return None — the caller polls via a oneshot.
        //
        // But the trait is sync. Instead, we use `try_send` on a tokio
        // blocking call: since the handle is behind a tokio mutex, we spawn
        // the async work on TOKIO and use a oneshot channel to get the result
        // back synchronously via `block_on`.
        //
        // Actually, the simplest correct approach: spawn on TOKIO, use a
        // std oneshot, and block on it via a tokio Enter guard. But blocking
        // on tokio from within tokio panics. Since this method is called
        // from the gpui main thread (not inside a tokio task), we can safely
        // use `TOKIO.block_on(...)`.
        let backend = TOKIO.block_on(self.new_channel_backend(cols, rows)).ok()?;
        Some(std::sync::Arc::new(backend))
    }
}

impl CrabPortMonitor for SshBackend {
    fn status(&self) -> RemoteStatus {
        self.monitor.read().status
    }

    fn metrics(&self) -> RemoteMetrics {
        self.monitor.read().metrics
    }
}

impl Drop for SshBackend {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Close);
    }
}

// ---------------------------------------------------------------------------
// Shell-history parsing
// ---------------------------------------------------------------------------

/// Parse the contents of a shell history file into a list of commands,
/// most-recent-first (the file's natural order is oldest-first, so we
/// reverse at the end).
///
/// Handles both formats we are likely to encounter:
///
/// - **bash** (`~/.bash_history`): one command per line, no metadata.
/// - **zsh** (`~/.zsh_history`): `: <epoch>:<duration>;<command>`, where
///   `<command>` may itself contain embedded newlines (each continuation
///   line is a raw command line, not prefixed with `:`).
///
/// Lines that fail to parse (or are empty after trimming) are skipped.
/// The result is capped at [`MAX_HISTORY`] entries to keep the UI panel
/// responsive on hosts with very large history files.
const MAX_HISTORY: usize = 1000;

fn parse_shell_history(raw: &str) -> Vec<String> {
    let mut cmds: Vec<String> = Vec::new();
    // Accumulator for the current zsh multi-line command.
    let mut current: Option<String> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix(':') {
            // Looks like a zsh meta line: `: <ts>:<dur>;<command>`.
            // Find the first `;` *after* the second `:` so commands
            // containing `;` aren't split.
            if let Some(semicolon) = rest.find(';') {
                // Flush any in-progress multi-line command first.
                if let Some(c) = current.take() {
                    push_cmd(&mut cmds, c);
                }
                let cmd = rest[semicolon + 1..].to_string();
                current = Some(cmd);
                continue;
            }
            // Malformed meta line — treat as plain text below.
        }
        if let Some(c) = current.as_mut() {
            // Continuation of a multi-line zsh command.
            c.push('\n');
            c.push_str(line);
        } else {
            // Plain bash-style line.
            push_cmd(&mut cmds, line.to_string());
        }
    }
    if let Some(c) = current.take() {
        push_cmd(&mut cmds, c);
    }
    // File is oldest-first; the UI wants most-recent-first.
    cmds.reverse();
    if cmds.len() > MAX_HISTORY {
        cmds.truncate(MAX_HISTORY);
    }
    cmds
}

fn push_cmd(out: &mut Vec<String>, s: String) {
    let trimmed = s.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
}
