//! Application-level shared state.
//!
//! All windows in the process share a single `AppState` via GPUI's global
//! mechanism (`cx.set_global` / `cx.global`). The main terminal window and
//! any auxiliary windows (Settings, About, ...) read/write through the same
//! `Store` handle so persistence stays consistent across windows.
//!
//! `Store` itself is `Send` but not `Sync` (rusqlite's `Connection` is not
//! `Sync`), so we wrap it in `parking_lot::Mutex`. The resulting
//! `Arc<Mutex<Store>>` is `Send + Sync` and can live in a GPUI `Global`.

use std::path::PathBuf;
use std::sync::Arc;

use fs2::FileExt;
use gpui::*;
use parking_lot::Mutex;

use crabport_core::store::Store;

use crate::windows::AuxWindowKind;

/// Process-wide shared state, reachable from any window via
/// `cx.global::<AppState>()`.
pub struct AppState {
    /// Shared persistent store. Lock briefly around each DB call.
    pub store: Arc<Mutex<Store>>,
}

impl Global for AppState {}

impl AppState {
    /// Open the store at the platform data directory and register the global.
    /// Called once from `main` during app bootstrap.
    pub fn init(cx: &mut App) {
        tracing::info!("app_state: initializing store");
        let store = match Store::open() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("app_state: failed to open store: {e}");
                panic!("failed to open store: {e}");
            }
        };
        cx.set_global(Self {
            store: Arc::new(Mutex::new(store)),
        });
    }

    /// Convenience accessor. Panics if `init` was not called yet — which is a
    /// programmer error (the global is set before any window is opened).
    pub fn store(cx: &App) -> Arc<Mutex<Store>> {
        cx.global::<Self>().store.clone()
    }

    /// Open (or focus) an auxiliary window of the given kind. Idempotent for
    /// singleton windows like Settings/About.
    pub fn focus_or_open(kind: AuxWindowKind, cx: &mut App) {
        crate::windows::focus_or_open(kind, cx);
    }

    /// Acquire the process-wide single-instance lock, or exit if another
    /// instance is already running.
    ///
    /// Locks the file `{data_dir}/crabport/.single.lock` exclusively for the
    /// lifetime of this process. The lock is `try_lock` (non-blocking): if the
    /// lock is already held, the second instance prints a message and exits.
    ///
    /// The returned `File` must be kept alive (dropped after the app exits) —
    /// `fs2` releases the lock on drop, so as long as the caller holds the
    /// guard the process is the sole instance.
    ///
    /// # Exit behavior
    ///
    /// On lock failure this function terminates the process via
    /// `std::process::exit(0)`. We exit with status 0 (success) because the
    /// second launch was not a crash — another instance simply took precedence.
    ///
    /// # Why a file lock instead of a PID file
    ///
    /// A plain PID file has a race: process A crashes, PID file lingers with
    /// a stale id, process B reads it and refuses to start even though A is
    /// gone. File locks are released by the OS as soon as the process exits
    /// (even on crash / SIGKILL), so they're always correct.
    pub fn acquire_single_instance_lock() -> std::fs::File {
        let dir = crabport_core::store::default_data_dir()
            .expect("cannot determine data dir for single-instance lock");
        std::fs::create_dir_all(&dir).ok();
        let lock_path: PathBuf = dir.join(".single.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap_or_else(|e| {
                panic!(
                    "failed to open single-instance lock at {}: {e}",
                    lock_path.display()
                );
            });
        if let Err(e) = file.try_lock_exclusive() {
            // Another instance already holds the lock. Tell the user (stderr
            // — no tracing subscriber is guaranteed to be initialized at the
            // call site yet) and exit silently. On macOS the existing
            // instance's `on_reopen` handler fires when the OS routes the
            // duplicate launch to the running process, so the user gets the
            // existing window back automatically.
            eprintln!("crabport: another instance is already running ({e}).");
            std::process::exit(0);
        }
        file
    }
}
