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

    fn sftp_entries(&self) -> Option<Arc<Vec<(String, bool)>>> {
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
                    kind: SftpTransferKind::Download,
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
