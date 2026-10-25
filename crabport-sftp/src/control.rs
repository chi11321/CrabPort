//! Cooperative pause / cancel + live counters for an in-flight transfer.
//!
//! ## Why this exists
//!
//! The SFTP transfer loops in [`crate::SftpBackend`] stream a file chunk by
//! chunk. Before this module they had no way to be interrupted: once a
//! transfer started, the only way to stop it was to close the tab (which
//! aborts the whole backend task). Each transfer now carries an optional
//! `Arc<TransferControl>`:
//!
//! - the streaming loop calls [`TransferControl::checkpoint`] before every
//!   chunk — it returns immediately while running, awaits while paused, and
//!   errors once cancelled;
//! - the backend mirrors byte counters and the current stage into it, so the
//!   UI can render a progress bar for a *specific* job without needing
//!   per-transfer ids on the shared event stream;
//! - [`TransferControl::finish`] records the outcome so the panel can retire
//!   the job even if it missed the broadcast event.
//!
//! ## Stage ids
//!
//! Stages are stored as `u8` constants rather than
//! `crabport_terminal::SftpTransferStage` because this crate sits *below*
//! `crabport-terminal` in the dependency graph. The UI maps them back to its
//! own enum for labels.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use tokio::sync::Notify;

/// Running normally — `checkpoint` returns immediately.
const RUNNING: u8 = 0;
/// Paused by the user — `checkpoint` waits for a resume (or cancel).
const PAUSED: u8 = 1;
/// Cancelled — `checkpoint` returns an error, ending the transfer.
const CANCELLED: u8 = 2;

/// No stage reported yet.
pub const STAGE_NONE: u8 = 0;
/// Local compression / remote `gzip` staging (no byte progress).
pub const STAGE_COMPRESS: u8 = 1;
/// The byte-carrying SFTP stream.
pub const STAGE_TRANSFER: u8 = 2;
/// Decompression after the bytes have arrived.
pub const STAGE_DECOMPRESS: u8 = 3;
/// Temp-file cleanup.
pub const STAGE_CLEANUP: u8 = 4;

/// Shared handle to one transfer's state.
///
/// Always held as an `Arc`; `Send + Sync` so it can travel into the backend's
/// spawned task and still be read by the UI thread.
pub struct TransferControl {
    /// One of `RUNNING` / `PAUSED` / `CANCELLED`.
    state: AtomicU8,
    /// One of the `STAGE_*` constants.
    stage: AtomicU8,
    /// Bytes processed in the current stage.
    done: AtomicU64,
    /// Total bytes for the current stage (0 = unknown).
    total: AtomicU64,
    /// Set once by [`Self::finish`].
    finished: AtomicBool,
    /// Outcome recorded by [`Self::finish`].
    success: AtomicBool,
    /// Human-readable result, set by [`Self::finish`].
    message: Mutex<String>,
    /// Wakes a paused loop on resume / cancel. `notify_one` (rather than
    /// `notify_waiters`) stores a permit when nobody is waiting, which closes
    /// the resume-before-await race.
    notify: Notify,
}

impl TransferControl {
    /// Create a control in the `RUNNING` state.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(RUNNING),
            stage: AtomicU8::new(STAGE_NONE),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            success: AtomicBool::new(false),
            message: Mutex::new(String::new()),
            notify: Notify::new(),
        })
    }

    /// Ask the transfer to pause. No-op when already paused, cancelled, or
    /// finished (a finished transfer has no loop left to pause).
    pub fn pause(&self) {
        let _ = self
            .state
            .compare_exchange(RUNNING, PAUSED, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Resume a paused transfer.
    pub fn resume(&self) {
        if self
            .state
            .compare_exchange(PAUSED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.notify.notify_one();
        }
    }

    /// Cancel the transfer. Idempotent; also wakes a paused loop so it can
    /// observe the cancellation.
    pub fn cancel(&self) {
        self.state.store(CANCELLED, Ordering::Release);
        self.notify.notify_one();
    }

    /// Toggle pause/resume and report the resulting paused state.
    pub fn toggle_pause(&self) -> bool {
        if self.is_paused() {
            self.resume();
            false
        } else {
            self.pause();
            true
        }
    }

    pub fn is_paused(&self) -> bool {
        self.state.load(Ordering::Acquire) == PAUSED
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == CANCELLED
    }

    /// Current stage id (`STAGE_*`).
    pub fn set_stage(&self, stage: u8) {
        self.stage.store(stage, Ordering::Release);
    }

    pub fn stage(&self) -> u8 {
        self.stage.load(Ordering::Acquire)
    }

    /// Mirror byte progress for the current stage.
    pub fn set_progress(&self, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// `(done, total)` for the current stage. `total == 0` means unknown.
    pub fn progress(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    /// Record the outcome. Called by the backend right before it broadcasts
    /// the finished event, so the panel can retire the job even if the event
    /// races the next render.
    pub fn finish(&self, success: bool, message: impl Into<String>) {
        *self.message.lock().unwrap_or_else(|e| e.into_inner()) = message.into();
        self.success.store(success, Ordering::Release);
        self.finished.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn success(&self) -> bool {
        self.success.load(Ordering::Acquire)
    }

    pub fn message(&self) -> String {
        self.message
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Cooperative interruption point for a streaming loop.
    ///
    /// Returns `Ok(())` immediately while running; waits while paused;
    /// returns an error once cancelled. Call it before each chunk so a pause
    /// takes effect within one chunk (256 KiB at most).
    pub async fn checkpoint(&self) -> Result<()> {
        loop {
            match self.state.load(Ordering::Acquire) {
                CANCELLED => return Err(anyhow!("transfer cancelled")),
                PAUSED => self.notify.notified().await,
                _ => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_and_resume_toggle_state() {
        let c = TransferControl::new();
        assert!(!c.is_paused());
        assert!(c.toggle_pause());
        assert!(c.is_paused());
        assert!(!c.toggle_pause());
        assert!(!c.is_paused());
    }

    #[test]
    fn cancel_is_sticky() {
        let c = TransferControl::new();
        assert!(!c.is_cancelled());
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn finish_records_outcome_once() {
        let c = TransferControl::new();
        assert!(!c.is_finished());
        c.finish(false, "boom");
        assert!(c.is_finished());
        assert!(!c.success());
        assert_eq!(c.message(), "boom");
    }

    #[test]
    fn progress_is_visible_to_readers() {
        let c = TransferControl::new();
        c.set_progress(512, 1024);
        assert_eq!(c.progress(), (512, 1024));
    }
}
