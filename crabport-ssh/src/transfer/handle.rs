use std::sync::Arc;

use async_broadcast::Sender as BroadcastSender;
use tokio::sync::Mutex as TokioMutex;

use crabport_sftp::CrabPortSftp;
use crabport_sftp::TransferControl;
use crabport_terminal::terminal::{
    BackendEvent, SftpTransferBytes, SftpTransferKind, SftpTransferStage,
};
use russh::client;

use crate::handler::SshHandler;

/// Lightweight borrowed view of the shared SFTP-related fields on
/// [`crate::SshBackend`].
///
/// This exists so the transfer orchestration can live in free functions
/// that take only what they need (the SSH handle + the SFTP session cache),
/// without borrowing `&SshBackend` — which would prevent the orchestration
/// from being awaited inside a `TOKIO.spawn` future (those require
/// `'static`).
pub(crate) struct SftpTransferHandle {
    pub(crate) handle: Arc<TokioMutex<Option<Arc<TokioMutex<client::Handle<SshHandler>>>>>>,
    pub(crate) sftp_session: Arc<TokioMutex<Option<crabport_sftp::SftpBackend>>>,
    /// Optional broadcast sink for live progress events. `None` when the
    /// handle is constructed by code paths that don't have an `event_tx`
    /// (e.g. tests); in that case progress is simply not reported.
    pub(crate) event_tx: Option<BroadcastSender<BackendEvent>>,
    /// Optional pause / cancel + progress mirror for the panel. `None` for
    /// transfers started through the legacy fire-and-forget methods (and for
    /// tests) — the transfer then runs to completion with no user control.
    pub(crate) control: Option<Arc<TransferControl>>,
}

/// Map the terminal crate's stage enum onto the control's `u8` stage ids.
/// Kept here (rather than in `crabport-sftp`) because that crate sits below
/// the one that defines the enum.
fn stage_id(stage: SftpTransferStage) -> u8 {
    match stage {
        SftpTransferStage::Compress => crabport_sftp::STAGE_COMPRESS,
        SftpTransferStage::Transfer => crabport_sftp::STAGE_TRANSFER,
        SftpTransferStage::Decompress => crabport_sftp::STAGE_DECOMPRESS,
        SftpTransferStage::CleanUp => crabport_sftp::STAGE_CLEANUP,
    }
}

impl SftpTransferHandle {
    /// Lazily open a fresh SFTP session when the cache is empty. Used by the
    /// transfer methods as a fallback — they prefer to reuse the cached
    /// session, but if a prior error dropped it we reconnect rather than
    /// failing the transfer outright.
    pub(crate) async fn open_sftp_session(&self) -> anyhow::Result<crabport_sftp::SftpBackend> {
        let guard = self.handle.lock().await;
        let shared = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SSH handle not available"))?;
        let h = shared.lock().await;
        crabport_sftp::SftpBackend::connect(&*h).await
    }

    /// Take the cached SFTP session if present, else open a fresh one.
    pub(crate) async fn take_or_open_sftp(&self) -> anyhow::Result<crabport_sftp::SftpBackend> {
        if let Some(s) = self.sftp_session.lock().await.take() {
            return Ok(s);
        }
        self.open_sftp_session().await
    }

    /// Return a live session to the cache. On error, close it instead so the
    /// cache doesn't hold a dead handle. Returns the original result for
    /// ergonomic chaining.
    pub(crate) async fn return_sftp(
        &self,
        s: crabport_sftp::SftpBackend,
        result: anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        match &result {
            Ok(()) => *self.sftp_session.lock().await = Some(s),
            Err(_) => {
                let _ = s.close().await;
            }
        }
        result
    }

    /// Best-effort broadcast of a transfer-progress event. Failures (e.g. no
    /// subscribers) are silently ignored — progress is informational, not
    /// load-bearing, and we must never let a UI-side drop affect the
    /// transfer itself.
    pub(crate) async fn emit_progress(
        &self,
        kind: SftpTransferKind,
        stage: SftpTransferStage,
        message: impl Into<String>,
    ) {
        self.emit_progress_bytes(kind, stage, message, None).await;
    }

    /// Like [`emit_progress`](Self::emit_progress) but carries byte-level
    /// progress for stages that support it (currently only the SFTP
    /// streaming `Transfer` stage).
    pub(crate) async fn emit_progress_bytes(
        &self,
        kind: SftpTransferKind,
        stage: SftpTransferStage,
        message: impl Into<String>,
        bytes: Option<SftpTransferBytes>,
    ) {
        let Some(tx) = self.event_tx.as_ref() else {
            return;
        };
        if let Some(c) = self.control.as_ref() {
            c.set_stage(stage_id(stage));
        }
        let _ = tx
            .broadcast(BackendEvent::SftpTransferProgress {
                kind,
                stage,
                message: message.into(),
                bytes,
            })
            .await;
    }

    /// Build a byte-progress callback suitable for handing to the SFTP
    /// streaming layer. The callback emits a `SftpTransferProgress` event
    /// with the given `(kind, stage, message)` and the current `(done, total)`
    /// byte counts. Throttled to one event per ~100ms so a fast transfer
    /// doesn't flood the broadcast channel.
    ///
    /// The returned closure is `Send + Sync` and cheap to clone (it holds an
    /// `Arc`), so it can be passed into `crabport-sftp`'s streaming functions.
    pub(crate) fn make_byte_progress_cb(
        &self,
        kind: SftpTransferKind,
        stage: SftpTransferStage,
        message: String,
        total: u64,
    ) -> Arc<dyn Fn(u64) + Send + Sync> {
        let tx = self.event_tx.clone();
        let control = self.control.clone();
        let last = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let message = std::sync::Arc::new(message);
        Arc::new(move |done: u64| {
            // Mirror every chunk into the control (unthrottled) so the
            // panel's progress bar is smooth even though the broadcast
            // event below is throttled.
            if let Some(c) = control.as_ref() {
                c.set_progress(done, total);
                c.set_stage(crabport_sftp::STAGE_TRANSFER);
            }
            // Throttle: only emit if at least 100ms of wall-clock has passed
            // since the last emit. We approximate "time" with a monotonic
            // nanos counter stored in the atomic — this avoids pulling in a
            // `Mutex<Instant>` just for throttling.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let prev = last.load(std::sync::atomic::Ordering::Relaxed);
            // 100ms throttle window.
            if now.saturating_sub(prev) < 100 && done != total {
                return;
            }
            last.store(now, std::sync::atomic::Ordering::Relaxed);
            let Some(tx) = tx.as_ref() else {
                return;
            };
            let bytes = SftpTransferBytes { done, total };
            let message = (*message).clone();
            // try_broadcast so we never block the streaming loop if the
            // channel is full — progress is best-effort.
            let _ = tx.try_broadcast(BackendEvent::SftpTransferProgress {
                kind,
                stage,
                message,
                bytes: Some(bytes),
            });
        })
    }
}
