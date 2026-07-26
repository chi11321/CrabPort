//! Auxiliary window registry + singleton window manager.
//!
//! `WindowRegistry` is stored as a GPUI global. It maps each
//! `AuxWindowKind` to a live `AnyWindowHandle`, so that [`focus_or_open`]
//! can de-duplicate windows (e.g. opening Settings when Settings is already
//! open just focuses it).
//!
//! Note: GPUI already has a `WindowKind` enum (`Normal` / `PopUp` /
//! `Floating`) describing system-level window behavior. Our enum is named
//! `AuxWindowKind` to avoid the collision — it's purely an
//! application-level label for which logical window this is.

use std::collections::HashMap;

use gpui::*;

/// Identifies an auxiliary (non-main) window type. Used as a singleton key
/// in `WindowRegistry`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum AuxWindowKind {
    Settings,
    About,
    ThemeEditor,
}

/// Tracks live auxiliary windows by kind so we can focus existing windows
/// instead of spawning duplicates.
///
/// Stored as a GPUI global via `cx.set_global`.
pub struct WindowRegistry {
    handles: HashMap<AuxWindowKind, AnyWindowHandle>,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
        }
    }

    /// Register or replace the handle for a given kind. Called by
    /// [`focus_or_open`] after a window is created.
    pub fn set(&mut self, kind: AuxWindowKind, handle: AnyWindowHandle) {
        self.handles.insert(kind, handle);
    }

    /// Look up the current handle for `kind`. Returns `None` if no window of
    /// that kind is tracked.
    pub fn get(&self, kind: AuxWindowKind) -> Option<AnyWindowHandle> {
        self.handles.get(&kind).copied()
    }

    /// Remove the handle for `kind`. Called when a window is closed.
    pub fn remove(&mut self, kind: AuxWindowKind) {
        self.handles.remove(&kind);
    }
}

impl Default for WindowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Open a window of the given `kind`, or focus it if one is already open.
///
/// Singleton semantics: at most one window per `AuxWindowKind`. This keeps
/// the UI predictable (no pile of Settings windows) and matches typical
/// desktop app behavior on macOS.
pub fn focus_or_open(kind: AuxWindowKind, cx: &mut App) {
    // Already open and still alive? Just focus it.
    let existing = cx
        .try_global::<WindowRegistry>()
        .and_then(|r| r.get(kind))
        .filter(|h| cx.windows().iter().any(|w| w.window_id() == h.window_id()));

    if let Some(handle) = existing {
        let _ = handle.update(cx, |_, window, _cx| window.activate_window());
        return;
    }

    // Open the window via the kind-specific constructor. We erase the root
    // view type to `AnyWindowHandle` so the registry is uniform.
    let handle: AnyWindowHandle = match kind {
        AuxWindowKind::Settings => crate::windows::SettingsWindow::open(cx).into(),
        AuxWindowKind::About => crate::windows::AboutWindow::open(cx).into(),
        AuxWindowKind::ThemeEditor => crate::windows::ThemeEditorWindow::open(cx).into(),
    };

    if cx.try_global::<WindowRegistry>().is_none() {
        cx.set_global(WindowRegistry::new());
    }
    cx.update_global::<WindowRegistry, _>(|r, _cx| r.set(kind, handle));
}
