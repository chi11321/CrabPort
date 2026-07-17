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

use std::sync::Arc;

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
}
