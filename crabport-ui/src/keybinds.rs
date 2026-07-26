//! Keybind catalog and registration.
//!
//! This module bridges the string-based keybind config (`config.toml`
//! `[keybinds]` section) with GPUI's typed `KeyBinding` system. Each
//! configurable action is listed in [`CATALOG`] with:
//!
//! - A stable `action_id` string (the config key)
//! - A display label (for the settings UI)
//! - A default keystroke string
//! - An optional key context (e.g. `"CrabPortTerminal"`)
//! - A builder closure that constructs the GPUI `KeyBinding` for a given
//!   keystroke string
//!
//! [`apply_bindings`] reads the config and registers all bindings with GPUI,
//! clearing any previous registrations first so runtime re-binding works.

use std::collections::BTreeMap;

use gpui::{App, KeyBinding};

use crabport_core::config;

// ---------------------------------------------------------------------------
// Catalog entry
// ---------------------------------------------------------------------------

/// One configurable keybind entry.
pub struct CatalogEntry {
    /// Stable identifier stored in `config.toml` under `[keybinds.bindings]`.
    pub action_id: &'static str,
    /// i18n key for the label shown in the Settings → Keybinds tab.
    pub label_key: &'static str,
    /// Default keystroke (used when no config override exists).
    pub default_keystroke: &'static str,
    /// Optional GPUI key context (e.g. `"CrabPortTerminal"`). `None` = global.
    pub context: Option<&'static str>,
    /// Whether this binding is shown in the Settings → Keybinds tab.
    /// `false` for system shortcuts (Quit, Hide, Minimize, etc.) that are
    /// still registered but not user-configurable.
    pub configurable: bool,
    /// Build a `KeyBinding` for the given keystroke string. The closure
    /// captures the action type at compile time.
    pub build: fn(&str) -> KeyBinding,
}

/// A convenient wrapper that bundles the resolved keystroke (after applying
/// config overrides) with the catalog entry, used by the settings UI.
pub struct ResolvedBinding {
    pub entry: CatalogEntry,
    pub keystroke: String,
}

/// Pick the macOS or non-macOS default keystroke at compile time. An empty
/// string means "no default binding" ([`apply_bindings`] skips empties).
fn plat(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

/// Expands one line per keybind into a full [`CatalogEntry`]:
/// `"id" => ActionType, context, default_keystroke, configurable`.
/// The settings label key is derived from the id
/// (`window.settings.keybinds.action_<id>`), and `context` feeds both the
/// entry field and the `KeyBinding` built by the closure, so the two can't
/// disagree.
macro_rules! entries {
    ( $( $id:literal => $action:path, $ctx:expr, $default:expr, $configurable:expr );+ $(;)? ) => {
        vec![$(
            CatalogEntry {
                action_id: $id,
                label_key: concat!("window.settings.keybinds.action_", $id),
                default_keystroke: $default,
                context: $ctx,
                configurable: $configurable,
                build: |ks| KeyBinding::new(ks, $action, $ctx),
            }
        ),+]
    };
}

/// GPUI key context for bindings scoped to a focused terminal.
const TERM: Option<&str> = Some("CrabPortTerminal");

/// The full list of configurable keybinds. Order is preserved for display.
/// Returned as a `Vec` because the default keystrokes are platform-
/// conditional (cfg!) which can't be used in a `const`/`static` context.
pub fn catalog() -> Vec<CatalogEntry> {
    entries!(
        // ---- App-level (configurable) ----
        "toggle_command" => crate::app::ToggleCommand, None, plat("cmd-k", "ctrl-k"), true;
        "open_settings" => crate::menus::OpenSettings, None, plat("cmd-,", "ctrl-,"), true;
        "open_about" => crate::menus::OpenAbout, None, plat("cmd-shift-a", "ctrl-shift-a"), true;
        // ---- App-level (not configurable, still registered) ----
        "quit" => crate::menus::Quit, None, plat("cmd-q", "ctrl-q"), false;
        "hide" => crate::menus::Hide, None, plat("cmd-h", ""), false;
        "minimize" => crate::menus::Minimize, None, plat("cmd-m", ""), false;
        "zoom" => crate::menus::Zoom, None, "", false;
        // ---- Terminal context (not configurable) ----
        "terminal_tab" => crate::app::TerminalTab, TERM, "tab", false;
        "terminal_shift_tab" => crate::app::TerminalShiftTab, TERM, "shift-tab", false;
        // ---- Terminal context (configurable) ----
        "terminal_increase_font" => crate::app::TerminalIncreaseFont, TERM, plat("cmd-=", "ctrl-="), true;
        "terminal_decrease_font" => crate::app::TerminalDecreaseFont, TERM, plat("cmd--", "ctrl--"), true;
        "terminal_reset_font" => crate::app::TerminalResetFont, TERM, plat("cmd-0", "ctrl-0"), true;
        "split_vertical" => crate::app::SplitVertical, TERM, plat("cmd-d", "ctrl-d"), true;
        "split_horizontal" => crate::app::SplitHorizontal, TERM, plat("cmd-shift-d", "ctrl-shift-d"), true;
    )
}

// ---------------------------------------------------------------------------
// Resolution + registration
// ---------------------------------------------------------------------------

/// Resolve the keystroke for a catalog entry, checking config overrides
/// first, then falling back to the default.
pub fn resolve_keystroke(entry: &CatalogEntry) -> String {
    let cfg = config::snapshot();
    cfg.keybinds
        .get(entry.action_id)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| entry.default_keystroke.to_string())
}

/// Resolve all catalog entries into (entry, keystroke) pairs.
pub fn resolve_all() -> Vec<ResolvedBinding> {
    catalog()
        .into_iter()
        .map(|entry| ResolvedBinding {
            keystroke: resolve_keystroke(&entry),
            entry,
        })
        .collect()
}

/// Clear all existing key bindings and re-register every catalog entry
/// from the current config. Call this at startup and whenever the user
/// changes a keybind in Settings.
pub fn apply_bindings(cx: &mut App) {
    cx.clear_key_bindings();
    let bindings: Vec<KeyBinding> = resolve_all()
        .into_iter()
        .filter(|rb| !rb.keystroke.is_empty())
        .map(|rb| (rb.entry.build)(&rb.keystroke))
        .collect();
    cx.bind_keys(bindings);
}

/// Persist a keybind override to config and re-apply all bindings.
/// An empty `keystroke` string disables the binding.
pub fn set_binding(action_id: &str, keystroke: &str, cx: &mut App) {
    let _ = config::update(|cfg| {
        cfg.keybinds.set(action_id, keystroke);
    });
    apply_bindings(cx);
}

/// Reset a single binding to its default.
pub fn reset_binding(action_id: &str, cx: &mut App) {
    let _ = config::update(|cfg| {
        cfg.keybinds.bindings.remove(action_id);
    });
    apply_bindings(cx);
}

/// Reset all bindings to their defaults.
pub fn reset_all_bindings(cx: &mut App) {
    let _ = config::update(|cfg| {
        cfg.keybinds = Default::default();
    });
    apply_bindings(cx);
}

/// Collect all bindings as a map of action_id → keystroke, for display.
pub fn bindings_map() -> BTreeMap<String, String> {
    resolve_all()
        .into_iter()
        .map(|rb| (rb.entry.action_id.to_string(), rb.keystroke))
        .collect()
}

// ---------------------------------------------------------------------------
// Keystroke recording (used by the Settings → Keybinds tab)
// ---------------------------------------------------------------------------

/// Convert a `KeyDownEvent` into the dash-joined keystroke string format
/// used by `KeyBinding::new` (e.g. `"cmd-shift-k"`).
///
/// Returns `None` for plain printable keys without modifiers (those are
/// not useful as app-level keybinds) and for keys whose resulting string
/// fails `Keystroke::parse` validation.
pub fn normalize_recorded_keystroke(event: &gpui::KeyDownEvent) -> Option<String> {
    let key = event.keystroke.key.trim();
    if key.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if event.keystroke.modifiers.control {
        parts.push("ctrl");
    }
    if event.keystroke.modifiers.alt {
        parts.push("alt");
    }
    if event.keystroke.modifiers.shift {
        parts.push("shift");
    }
    if event.keystroke.modifiers.platform {
        parts.push("cmd");
    }
    if event.keystroke.modifiers.function {
        parts.push("fn");
    }

    // Bare printable chars (no modifiers) are not useful as app-level
    // keybinds — skip them.
    if parts.is_empty() {
        match key {
            "escape" | "enter" | "tab" | "backspace" | "delete" | "home" | "end" | "pageup"
            | "pagedown" | "up" | "down" | "left" | "right" => {}
            _ => return None,
        }
    }

    parts.push(key);
    let keystroke = parts.join("-");
    gpui::Keystroke::parse(&keystroke).ok().map(|_| keystroke)
}

/// Check if a keystroke conflicts with any other action's binding.
/// Returns `Some((action_id, label_key))` if there is a conflict.
pub fn find_conflict(current_action_id: &str, new_keystroke: &str) -> Option<(String, String)> {
    for rb in resolve_all() {
        if rb.entry.action_id == current_action_id {
            continue;
        }
        if !rb.keystroke.is_empty() && rb.keystroke == new_keystroke {
            return Some((
                rb.entry.action_id.to_string(),
                rb.entry.label_key.to_string(),
            ));
        }
    }
    None
}
