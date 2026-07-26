//! Runtime-configurable color theme.
//!
//! Colors live in `crabport_core::config::ThemeConfig` (serialized to
//! `[appearance.theme]` in `config.toml`) as human-readable hex strings. This
//! module parses them into `u32` (`0xRRGGBBAA`) and exposes snake_case
//! accessors — e.g. `color::bg_base()` — that every UI surface calls.
//!
//! `refresh_theme()` reloads the live config into the cached [`Theme`] so
//! changes from the Settings window (or an external `config.toml` edit
//! followed by `refresh_theme`) take effect immediately. Callers that need a
//! fully consistent snapshot across a render should grab `theme()` and read
//! fields off it.
//!
//! The default palette is "modern-dark" — a refined, slightly cool neutral
//! dark with an indigo accent and well-tuned neutrals. Other built-in
//! presets (mocha, tokyo-night) are selectable from Settings.

use gpui::{Rgba, rgb};
use parking_lot::RwLock;
use std::sync::LazyLock;

use crabport_core::config::{self, ThemeConfig};

// ---------------------------------------------------------------------------
// Parsed theme
// ---------------------------------------------------------------------------

/// Declares the [`Theme`] struct, its `from_config` parser, and one public
/// snake_case accessor per field — all generated from the single
/// `name => config.path` list below so the three can never drift apart.
///
/// - The struct holds every color parsed to `u32` in `0xRRGGBBAA` form. Built
///   once from a [`ThemeConfig`] and cached in [`THEME`]. Cheap to clone
///   (just `u32`s), so `theme()` hands out copies freely.
/// - `from_config` parses each hex string; malformed values fall back to the
///   matching field of `ThemeConfig::modern_dark()` (which itself must parse —
///   panicking there surfaces a build-time bug in the preset rather than
///   silently rendering black), so a single bad value in `config.toml` can't
///   brick the UI.
/// - Render code calls the accessors (`color::bg_base()` etc.); each is just
///   `*THEME.read()` + a field read — a handful of ns. We don't expose the
///   `Theme` directly to call sites because the accessor form keeps the
///   "always reflects the latest config" invariant local to this module.
macro_rules! theme_fields {
    ( $( $name:ident => $section:ident . $field:ident ),+ $(,)? ) => {
        #[derive(Clone, Copy, Debug)]
        pub struct Theme {
            $( pub $name: u32, )+
        }

        impl Theme {
            /// Parse a [`ThemeConfig`] into `u32` values, falling back to
            /// `ThemeConfig::modern_dark()` per malformed field.
            pub fn from_config(cfg: &ThemeConfig) -> Self {
                let fallback = ThemeConfig::modern_dark();
                Self {
                    $( $name: parse_hex(&cfg.$section.$field).unwrap_or_else(|| {
                        parse_hex(&fallback.$section.$field)
                            .expect("modern-dark preset must parse")
                    }), )+
                }
            }
        }

        $( pub fn $name() -> u32 { theme().$name } )+
    };
}

theme_fields!(
    // Base
    bg_base => base.bg_base,
    bg_sidebar => base.bg_sidebar,
    bg_tab_bar => base.bg_tab_bar,
    // Border
    border => border.border,
    // Surface
    surface_hover => surface.surface_hover,
    surface_active => surface.surface_active,
    // Text
    text_primary => text.text_primary,
    text_muted => text.text_muted,
    // Tab button
    tab_btn_bg => tab_button.bg,
    tab_btn_bg_hover => tab_button.bg_hover,
    tab_btn_bg_selected => tab_button.bg_selected,
    tab_btn_bg_pressed => tab_button.bg_pressed,
    tab_btn_bg_disabled => tab_button.bg_disabled,
    tab_btn_border => tab_button.border,
    tab_btn_text_disabled => tab_button.text_disabled,
    // Button
    btn_bg => button.bg,
    btn_bg_hover => button.bg_hover,
    btn_bg_selected => button.bg_selected,
    btn_bg_pressed => button.bg_pressed,
    btn_bg_disabled => button.bg_disabled,
    btn_border => button.border,
    btn_text_disabled => button.text_disabled,
    // Button — primary
    btn_primary_bg => button_primary.bg,
    btn_primary_bg_hover => button_primary.bg_hover,
    btn_primary_bg_selected => button_primary.bg_selected,
    btn_primary_bg_disabled => button_primary.bg_disabled,
    btn_primary_border => button_primary.border,
    btn_primary_text_disabled => button_primary.text_disabled,
    // Button — ghost
    btn_ghost_bg => button_ghost.bg,
    btn_ghost_bg_hover => button_ghost.bg_hover,
    btn_ghost_bg_selected => button_ghost.bg_selected,
    btn_ghost_bg_disabled => button_ghost.bg_disabled,
    btn_ghost_border => button_ghost.border,
    btn_ghost_text_disabled => button_ghost.text_disabled,
    // Input
    input_bg => input.bg,
    input_bg_focused => input.bg_focused,
    input_bg_disabled => input.bg_disabled,
    input_text_disabled => input.text_disabled,
    input_border => input.border,
    input_border_hover => input.border_hover,
    input_border_focused => input.border_focused,
    input_border_error => input.border_error,
    input_placeholder => input.placeholder,
    input_selection => input.selection,
    // Command
    command_overlay => command.overlay,
    command_bg => command.bg,
    command_border => command.border,
    command_item_hover => command.item_hover,
    command_item_active => command.item_active,
    command_group_label => command.group_label,
    // Terminal ANSI
    term_fg => terminal.fg,
    term_bg => terminal.bg,
    term_cursor => terminal.cursor,
    term_black => terminal.black,
    term_red => terminal.red,
    term_green => terminal.green,
    term_yellow => terminal.yellow,
    term_blue => terminal.blue,
    term_magenta => terminal.magenta,
    term_cyan => terminal.cyan,
    term_white => terminal.white,
    term_bright_black => terminal.bright_black,
    term_bright_red => terminal.bright_red,
    term_bright_green => terminal.bright_green,
    term_bright_yellow => terminal.bright_yellow,
    term_bright_blue => terminal.bright_blue,
    term_bright_magenta => terminal.bright_magenta,
    term_bright_cyan => terminal.bright_cyan,
    term_bright_white => terminal.bright_white,
    // Selection
    selection_bg => selection.bg,
);

/// Parse a hex color string into the `u32` form expected by GPUI's
/// `rgb()` / `rgba()`.
///
/// Accepted forms (case-insensitive):
/// - `"#RRGGBB"`, `"RRGGBB"` → `0x00RRGGBB` (high byte zero so `rgb()`
///   drops it and reads R/G/B).
/// - `"#RRGGBBAA"`, `"RRGGBBAA"` → `0xRRGGBBAA` (caller routes through
///   `rgba()`, e.g. via the `to_color` helper in `button.rs`).
///
/// We deliberately do **not** synthesize an alpha channel: GPUI's
/// `rgb(hex)` discards the *high* byte (`let [_, r, g, b] =
/// hex.to_be_bytes()`), so a 6-digit color must stay `0x00RRGGBB`. Padding
/// it to `0xRRGGBBff` would shift the channels and turn the background
///
/// Returns `None` for anything that isn't 6 or 8 hex digits (after an
/// optional `#`). A `None` result lets [`Theme::from_config`] substitute
/// the modern-dark fallback so the UI never breaks.
pub fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 {
        u32::from_str_radix(s, 16).ok()
    } else if s.len() == 8 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Cached global theme
// ---------------------------------------------------------------------------

/// Process-wide parsed theme, initialized from `config.toml` on first
/// access. [`refresh_theme`] re-reads the live config into this cache.
static THEME: LazyLock<RwLock<Theme>> =
    LazyLock::new(|| RwLock::new(Theme::from_config(&config::snapshot().appearance.theme)));

/// Take a read lock and return a snapshot of the current theme. Cheap
/// (struct-of-`u32` copy) — call freely from render paths.
pub fn theme() -> Theme {
    *THEME.read()
}

/// Process-wide flag set once on macOS when the main / settings / about windows
/// are opened with `WindowBackgroundAppearance::Blurred`. When true, sidebar
/// surfaces paint a semi-transparent tint instead of an opaque fill so the
/// system-provided vibrancy layer (an `NSVisualEffectView`) shows through,
/// producing the macOS "sidebar 毛玻璃" look. On other platforms this is
/// always false and sidebars stay fully opaque.
#[cfg(target_os = "macos")]
static VIBRANCY_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Mark vibrancy as on. Called once from `main.rs` on macOS after the
/// `gpui-component` theme is initialized.
///
/// This does two things:
/// 1. Sets the process-wide flag so [`sidebar_bg_color`] returns a
///    translucent tint on subsequent renders.
/// 2. Patches the `gpui-component` global [`Theme`]'s `background` to be
///    fully transparent. `gpui_component::Root` paints an opaque
///    `.bg(cx.theme().background)` over the whole window on every frame,
///    which would otherwise mask the system vibrancy layer. Since this app
///    never re-calls `Theme::change` at runtime (it drives its own colors via
///    the `crabport_ui::color` module), the patch persists for the process
///    lifetime.
#[cfg(target_os = "macos")]
pub fn enable_vibrancy(cx: &mut gpui::App) {
    VIBRANCY_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    if cx.has_global::<gpui_component::Theme>() {
        let theme = gpui_component::Theme::global_mut(cx);
        theme.colors.background = theme.colors.background.alpha(0.3);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn enable_vibrancy(_cx: &mut gpui::App) {}

/// Background color to use on sidebar-like surfaces.
/// On macOS when vibrancy is enabled, returns the configured sidebar tint
/// at ~55% alpha so the system vibrancy layer reads through (Finder/Mail
/// sidebar style). Otherwise returns the opaque sidebar color as a GPUI
/// `Rgba` ready for `.bg(...)`.
///
/// We go through `rgba()` (not `rgb()`) so the alpha byte is honored: GPUI's
/// `rgb(hex)` discards the high byte, which would silently drop the alpha we
/// pack in. Callers that previously wrote `.bg(rgb(bg_sidebar()))` should call
/// `.bg(sidebar_bg_color())` instead.
pub fn sidebar_bg_color() -> Rgba {
    let t = theme();
    #[cfg(target_os = "macos")]
    if VIBRANCY_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        // `bg_sidebar` is stored as `0x00RRGGBB` (6-digit form). Shift it into
        // `0xRRGGBBAA` with alpha ~0.55 (0x8C) — enough tint to keep contrast
        // for text while letting the vibrancy dominate.
        use gpui::rgba;
        let rgb_only = t.bg_sidebar & 0x00FF_FFFF;
        return rgba((rgb_only << 8) | 0x8C);
    }
    let _ = t;
    rgb(bg_sidebar())
}

/// Background color for the default (unselected / unhovered) state of a
/// `.tab()` button.
///
/// On macOS when vibrancy is on, returns a fully transparent color so the
/// button doesn't paint an opaque fill over the sidebar vibrancy — the
/// "毛玻璃" reads straight through the button (Finder/Mail sidebar style).
/// Hover / selected states keep their own colors, so the transition from
/// transparent → opaque on hover reads as a fade-in, matching macOS native
/// sidebar buttons.
///
/// On other platforms (or macOS with vibrancy off) this is just the opaque
/// `tab_btn_bg`.
pub fn tab_btn_bg_color() -> u32 {
    #[cfg(target_os = "macos")]
    if VIBRANCY_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        // Pack RGB + alpha 0 into 0xRRGGBBAA. `Button::render` routes this
        // through `rgba()` (via `to_color`), so the alpha byte is honored.
        let rgb_only = tab_btn_bg() & 0x00FF_FFFF;
        return (rgb_only << 8) | 0x00;
    }
    tab_btn_bg()
}

/// Fully-opaque base background, used for the content area so it masks the
/// vibrancy layer everywhere *except* the sidebar. Kept as a helper (rather
/// than inlining `rgb(bg_base())`) so the vibrancy boundary is greppable in
/// one place.
pub fn opaque_base_bg() -> Rgba {
    rgb(bg_base())
}

/// Load `cfg` into the cached [`Theme`] **without** touching `config.toml`.
/// Used by the Theme Editor for live preview: every window repaints with the
/// in-progress palette, and a later [`refresh_theme`] (e.g. when the editor
/// closes without saving) restores the persisted theme.
pub fn preview_theme(cfg: &ThemeConfig) {
    *THEME.write() = Theme::from_config(cfg);
}

/// Re-read the live `config.toml` theme into the cached [`Theme`]. Call this
/// after mutating `config::update(|cfg| cfg.appearance.theme = ...)` so every
/// subsequent `color::*()` accessor reflects the new values.
pub fn refresh_theme() {
    let snapshot = config::snapshot();
    let mut guard = THEME.write();
    *guard = Theme::from_config(&snapshot.appearance.theme);
}

/// Apply a preset by id and persist it. Convenience wrapper for the Settings
/// window: writes the preset to config, refreshes the cache, and returns the
/// new theme so the caller can drive a global repaint.
///
/// Kept for backwards-compat with callers that only know the built-in
/// preset ids; new code should prefer [`apply_theme`] which resolves any
/// id (built-in OR custom) via the theme catalog.
pub fn apply_preset(id: &str) -> Theme {
    let _ = config::update(|cfg| {
        cfg.appearance.theme = ThemeConfig::preset(id);
    });
    refresh_theme();
    theme()
}

/// Apply a theme by id (built-in OR custom) and persist it. Resolves `id`
/// via the [`crate::theme`] catalog — which merges the embedded built-in
/// themes with user-supplied `.toml` files from `{data_dir}/crabport/themes/`
/// — writes the resolved [`ThemeConfig`] to `config.toml`, refreshes the
/// cached parsed [`Theme`], and returns the new parsed theme so the caller
/// can drive a global repaint.
///
/// Unknown ids fall back to `modern-dark` (see [`crate::theme::get`]), so a
/// stale `config.toml` referencing a deleted custom theme can never break
/// the UI.
pub fn apply_theme(id: &str) -> Theme {
    let cfg = crate::theme::get(id);
    let _ = config::update(|c| {
        c.appearance.theme = cfg;
    });
    refresh_theme();
    theme()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_accepts_6_and_8_digit_forms() {
        assert_eq!(parse_hex("#14161c"), Some(0x0014161c));
        assert_eq!(parse_hex("14161c"), Some(0x0014161c));
        assert_eq!(parse_hex("  #FFAA00  "), Some(0x00ffaa00));
        // 8-digit keeps the alpha byte in the low position (0xRRGGBBAA).
        assert_eq!(parse_hex("#818cf833"), Some(0x818cf833));
        assert_eq!(parse_hex("00000000"), Some(0x00000000));
    }

    #[test]
    fn parse_hex_rejects_everything_else() {
        for bad in [
            "",
            "#",
            "#fff",
            "#12345",
            "#1234567",
            "#123456789",
            "zzzzzz",
            "#gggggg",
        ] {
            assert_eq!(parse_hex(bad), None, "input {bad:?}");
        }
    }

    #[test]
    fn from_config_falls_back_per_field_on_bad_values() {
        let fallback = Theme::from_config(&ThemeConfig::modern_dark());

        let mut cfg = ThemeConfig::modern_dark();
        cfg.base.bg_base = "not-a-color".into(); // malformed
        cfg.text.text_primary = String::new(); // empty (theme file omitted it)
        cfg.terminal.red = "#ff0000".into(); // valid override

        let theme = Theme::from_config(&cfg);
        // Bad/missing fields inherit modern-dark…
        assert_eq!(theme.bg_base, fallback.bg_base);
        assert_eq!(theme.text_primary, fallback.text_primary);
        // …while valid fields apply, and neighbors are unaffected.
        assert_eq!(theme.term_red, 0x00ff0000);
        assert_eq!(theme.border, fallback.border);
    }
}
