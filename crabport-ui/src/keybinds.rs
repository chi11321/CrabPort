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

/// The full list of configurable keybinds. Order is preserved for display.
/// Returned as a `Vec` because the default keystrokes are platform-
/// conditional (cfg!) which can't be used in a `const`/`static` context.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        // ---- App-level (configurable) ----
        CatalogEntry {
            action_id: "toggle_command",
            label_key: "window.settings.keybinds.action_toggle_command",
            default_keystroke: default_toggle_command(),
            context: None,
            configurable: true,
            build: |ks| KeyBinding::new(ks, crate::app::ToggleCommand, None),
        },
        CatalogEntry {
            action_id: "open_settings",
            label_key: "window.settings.keybinds.action_open_settings",
            default_keystroke: default_open_settings(),
            context: None,
            configurable: true,
            build: |ks| KeyBinding::new(ks, crate::menus::OpenSettings, None),
        },
        CatalogEntry {
            action_id: "open_about",
            label_key: "window.settings.keybinds.action_open_about",
            default_keystroke: default_open_about(),
            context: None,
            configurable: true,
            build: |ks| KeyBinding::new(ks, crate::menus::OpenAbout, None),
        },
        // ---- App-level (not configurable, still registered) ----
        CatalogEntry {
            action_id: "quit",
            label_key: "window.settings.keybinds.action_quit",
            default_keystroke: default_quit(),
            context: None,
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::menus::Quit, None),
        },
        CatalogEntry {
            action_id: "hide",
            label_key: "window.settings.keybinds.action_hide",
            default_keystroke: default_hide(),
            context: None,
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::menus::Hide, None),
        },
        CatalogEntry {
            action_id: "minimize",
            label_key: "window.settings.keybinds.action_minimize",
            default_keystroke: default_minimize(),
            context: None,
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::menus::Minimize, None),
        },
        CatalogEntry {
            action_id: "zoom",
            label_key: "window.settings.keybinds.action_zoom",
            default_keystroke: default_zoom(),
            context: None,
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::menus::Zoom, None),
        },
        // ---- Terminal context (not configurable) ----
        CatalogEntry {
            action_id: "terminal_tab",
            label_key: "window.settings.keybinds.action_terminal_tab",
            default_keystroke: "tab",
            context: Some("CrabPortTerminal"),
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::app::TerminalTab, Some("CrabPortTerminal")),
        },
        CatalogEntry {
            action_id: "terminal_shift_tab",
            label_key: "window.settings.keybinds.action_terminal_shift_tab",
            default_keystroke: "shift-tab",
            context: Some("CrabPortTerminal"),
            configurable: false,
            build: |ks| KeyBinding::new(ks, crate::app::TerminalShiftTab, Some("CrabPortTerminal")),
        },
        // ---- Terminal context (configurable) ----
        CatalogEntry {
            action_id: "terminal_increase_font",
            label_key: "window.settings.keybinds.action_terminal_increase_font",
            default_keystroke: default_font_zoom(),
            context: Some("CrabPortTerminal"),
            configurable: true,
            build: |ks| {
                KeyBinding::new(
                    ks,
                    crate::app::TerminalIncreaseFont,
                    Some("CrabPortTerminal"),
                )
            },
        },
        CatalogEntry {
            action_id: "terminal_decrease_font",
            label_key: "window.settings.keybinds.action_terminal_decrease_font",
            default_keystroke: default_font_shrink(),
            context: Some("CrabPortTerminal"),
            configurable: true,
            build: |ks| {
                KeyBinding::new(
                    ks,
                    crate::app::TerminalDecreaseFont,
                    Some("CrabPortTerminal"),
                )
            },
        },
        CatalogEntry {
            action_id: "terminal_reset_font",
            label_key: "window.settings.keybinds.action_terminal_reset_font",
            default_keystroke: default_font_reset(),
            context: Some("CrabPortTerminal"),
            configurable: true,
            build: |ks| {
                KeyBinding::new(ks, crate::app::TerminalResetFont, Some("CrabPortTerminal"))
            },
        },
        CatalogEntry {
            action_id: "split_vertical",
            label_key: "window.settings.keybinds.action_split_vertical",
            default_keystroke: default_split_vertical(),
            context: Some("CrabPortTerminal"),
            configurable: true,
            build: |ks| KeyBinding::new(ks, crate::app::SplitVertical, Some("CrabPortTerminal")),
        },
        CatalogEntry {
            action_id: "split_horizontal",
            label_key: "window.settings.keybinds.action_split_horizontal",
            default_keystroke: default_split_horizontal(),
            context: Some("CrabPortTerminal"),
            configurable: true,
            build: |ks| KeyBinding::new(ks, crate::app::SplitHorizontal, Some("CrabPortTerminal")),
        },
    ]
}

// ---------------------------------------------------------------------------
// Platform-conditional defaults
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn default_toggle_command() -> &'static str {
    "cmd-k"
}
#[cfg(not(target_os = "macos"))]
fn default_toggle_command() -> &'static str {
    "ctrl-k"
}

#[cfg(target_os = "macos")]
fn default_open_settings() -> &'static str {
    "cmd-,"
}
#[cfg(not(target_os = "macos"))]
fn default_open_settings() -> &'static str {
    "ctrl-,"
}

#[cfg(target_os = "macos")]
fn default_open_about() -> &'static str {
    "cmd-shift-a"
}
#[cfg(not(target_os = "macos"))]
fn default_open_about() -> &'static str {
    "ctrl-shift-a"
}

#[cfg(target_os = "macos")]
fn default_quit() -> &'static str {
    "cmd-q"
}
#[cfg(not(target_os = "macos"))]
fn default_quit() -> &'static str {
    "ctrl-q"
}

#[cfg(target_os = "macos")]
fn default_hide() -> &'static str {
    "cmd-h"
}
#[cfg(not(target_os = "macos"))]
fn default_hide() -> &'static str {
    ""
}

#[cfg(target_os = "macos")]
fn default_minimize() -> &'static str {
    "cmd-m"
}
#[cfg(not(target_os = "macos"))]
fn default_minimize() -> &'static str {
    ""
}

#[cfg(target_os = "macos")]
fn default_zoom() -> &'static str {
    ""
}
#[cfg(not(target_os = "macos"))]
fn default_zoom() -> &'static str {
    ""
}

#[cfg(target_os = "macos")]
fn default_font_zoom() -> &'static str {
    "cmd-="
}
#[cfg(not(target_os = "macos"))]
fn default_font_zoom() -> &'static str {
    "ctrl-="
}

#[cfg(target_os = "macos")]
fn default_font_shrink() -> &'static str {
    "cmd--"
}
#[cfg(not(target_os = "macos"))]
fn default_font_shrink() -> &'static str {
    "ctrl--"
}

#[cfg(target_os = "macos")]
fn default_font_reset() -> &'static str {
    "cmd-0"
}
#[cfg(not(target_os = "macos"))]
fn default_font_reset() -> &'static str {
    "ctrl-0"
}

#[cfg(target_os = "macos")]
fn default_split_vertical() -> &'static str {
    "cmd-d"
}
#[cfg(not(target_os = "macos"))]
fn default_split_vertical() -> &'static str {
    "ctrl-d"
}

#[cfg(target_os = "macos")]
fn default_split_horizontal() -> &'static str {
    "cmd-shift-d"
}
#[cfg(not(target_os = "macos"))]
fn default_split_horizontal() -> &'static str {
    "ctrl-shift-d"
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
