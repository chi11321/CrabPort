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

/// All theme colors parsed to `u32` in `0xRRGGBBAA` form. Built once from a
/// [`ThemeConfig`] and cached in [`THEME`]. Cheap to clone (just a struct of
/// `u32`s), so `theme()` hands out copies freely.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    // Base
    pub bg_base: u32,
    pub bg_sidebar: u32,
    pub bg_tab_bar: u32,

    // Border
    pub border: u32,

    // Surface
    pub surface_hover: u32,
    pub surface_active: u32,

    // Text
    pub text_primary: u32,
    pub text_muted: u32,

    // Tab button
    pub tab_btn_bg: u32,
    pub tab_btn_bg_hover: u32,
    pub tab_btn_bg_selected: u32,
    pub tab_btn_bg_pressed: u32,
    pub tab_btn_bg_disabled: u32,
    pub tab_btn_border: u32,
    pub tab_btn_text_disabled: u32,

    // Button
    pub btn_bg: u32,
    pub btn_bg_hover: u32,
    pub btn_bg_selected: u32,
    pub btn_bg_pressed: u32,
    pub btn_bg_disabled: u32,
    pub btn_border: u32,
    pub btn_text_disabled: u32,

    // Button — primary
    pub btn_primary_bg: u32,
    pub btn_primary_bg_hover: u32,
    pub btn_primary_bg_selected: u32,
    pub btn_primary_bg_disabled: u32,
    pub btn_primary_border: u32,
    pub btn_primary_text_disabled: u32,

    // Button — ghost
    pub btn_ghost_bg: u32,
    pub btn_ghost_bg_hover: u32,
    pub btn_ghost_bg_selected: u32,
    pub btn_ghost_bg_disabled: u32,
    pub btn_ghost_border: u32,
    pub btn_ghost_text_disabled: u32,

    // Input
    pub input_bg: u32,
    pub input_bg_focused: u32,
    pub input_bg_disabled: u32,
    pub input_text_disabled: u32,
    pub input_border: u32,
    pub input_border_hover: u32,
    pub input_border_focused: u32,
    pub input_border_error: u32,
    pub input_placeholder: u32,
    pub input_selection: u32,

    // Command
    pub command_overlay: u32,
    pub command_bg: u32,
    pub command_border: u32,
    pub command_item_hover: u32,
    pub command_item_active: u32,
    pub command_group_label: u32,

    // Terminal ANSI
    pub term_fg: u32,
    pub term_bg: u32,
    pub term_cursor: u32,
    pub term_black: u32,
    pub term_red: u32,
    pub term_green: u32,
    pub term_yellow: u32,
    pub term_blue: u32,
    pub term_magenta: u32,
    pub term_cyan: u32,
    pub term_white: u32,
    pub term_bright_black: u32,
    pub term_bright_red: u32,
    pub term_bright_green: u32,
    pub term_bright_yellow: u32,
    pub term_bright_blue: u32,
    pub term_bright_magenta: u32,
    pub term_bright_cyan: u32,
    pub term_bright_white: u32,
    pub selection_bg: u32,
}

impl Theme {
    /// Parse a [`ThemeConfig`] into `u32` values. Malformed hex strings fall
    /// back to the matching field of `ThemeConfig::modern_dark()` so a single
    /// bad value in `config.toml` can't brick the UI.
    pub fn from_config(cfg: &ThemeConfig) -> Self {
        let fallback = ThemeConfig::modern_dark();
        // `$cfg` is an expression yielding `&String`; `$fb` is the matching
        // modern-dark field. Unparseable `cfg` value → fall back to `fb`,
        // which itself must parse (panicking here surfaces a build-time bug
        // in the modern-dark preset rather than silently rendering black).
        macro_rules! p {
            ($cfg:expr, $fb:expr) => {
                parse_hex($cfg)
                    .unwrap_or_else(|| parse_hex($fb).expect("modern-dark preset must parse"))
            };
        }
        Self {
            bg_base: p!(&cfg.base.bg_base, &fallback.base.bg_base),
            bg_sidebar: p!(&cfg.base.bg_sidebar, &fallback.base.bg_sidebar),
            bg_tab_bar: p!(&cfg.base.bg_tab_bar, &fallback.base.bg_tab_bar),
            border: p!(&cfg.border.border, &fallback.border.border),
            surface_hover: p!(&cfg.surface.surface_hover, &fallback.surface.surface_hover),
            surface_active: p!(
                &cfg.surface.surface_active,
                &fallback.surface.surface_active
            ),
            text_primary: p!(&cfg.text.text_primary, &fallback.text.text_primary),
            text_muted: p!(&cfg.text.text_muted, &fallback.text.text_muted),
            tab_btn_bg: p!(&cfg.tab_button.bg, &fallback.tab_button.bg),
            tab_btn_bg_hover: p!(&cfg.tab_button.bg_hover, &fallback.tab_button.bg_hover),
            tab_btn_bg_selected: p!(
                &cfg.tab_button.bg_selected,
                &fallback.tab_button.bg_selected
            ),
            tab_btn_bg_pressed: p!(&cfg.tab_button.bg_pressed, &fallback.tab_button.bg_pressed),
            tab_btn_bg_disabled: p!(
                &cfg.tab_button.bg_disabled,
                &fallback.tab_button.bg_disabled
            ),
            tab_btn_border: p!(&cfg.tab_button.border, &fallback.tab_button.border),
            tab_btn_text_disabled: p!(
                &cfg.tab_button.text_disabled,
                &fallback.tab_button.text_disabled
            ),
            btn_bg: p!(&cfg.button.bg, &fallback.button.bg),
            btn_bg_hover: p!(&cfg.button.bg_hover, &fallback.button.bg_hover),
            btn_bg_selected: p!(&cfg.button.bg_selected, &fallback.button.bg_selected),
            btn_bg_pressed: p!(&cfg.button.bg_pressed, &fallback.button.bg_pressed),
            btn_bg_disabled: p!(&cfg.button.bg_disabled, &fallback.button.bg_disabled),
            btn_border: p!(&cfg.button.border, &fallback.button.border),
            btn_text_disabled: p!(&cfg.button.text_disabled, &fallback.button.text_disabled),
            btn_primary_bg: p!(&cfg.button_primary.bg, &fallback.button_primary.bg),
            btn_primary_bg_hover: p!(
                &cfg.button_primary.bg_hover,
                &fallback.button_primary.bg_hover
            ),
            btn_primary_bg_selected: p!(
                &cfg.button_primary.bg_selected,
                &fallback.button_primary.bg_selected
            ),
            btn_primary_bg_disabled: p!(
                &cfg.button_primary.bg_disabled,
                &fallback.button_primary.bg_disabled
            ),
            btn_primary_border: p!(&cfg.button_primary.border, &fallback.button_primary.border),
            btn_primary_text_disabled: p!(
                &cfg.button_primary.text_disabled,
                &fallback.button_primary.text_disabled
            ),
            btn_ghost_bg: p!(&cfg.button_ghost.bg, &fallback.button_ghost.bg),
            btn_ghost_bg_hover: p!(&cfg.button_ghost.bg_hover, &fallback.button_ghost.bg_hover),
            btn_ghost_bg_selected: p!(
                &cfg.button_ghost.bg_selected,
                &fallback.button_ghost.bg_selected
            ),
            btn_ghost_bg_disabled: p!(
                &cfg.button_ghost.bg_disabled,
                &fallback.button_ghost.bg_disabled
            ),
            btn_ghost_border: p!(&cfg.button_ghost.border, &fallback.button_ghost.border),
            btn_ghost_text_disabled: p!(
                &cfg.button_ghost.text_disabled,
                &fallback.button_ghost.text_disabled
            ),
            input_bg: p!(&cfg.input.bg, &fallback.input.bg),
            input_bg_focused: p!(&cfg.input.bg_focused, &fallback.input.bg_focused),
            input_bg_disabled: p!(&cfg.input.bg_disabled, &fallback.input.bg_disabled),
            input_text_disabled: p!(&cfg.input.text_disabled, &fallback.input.text_disabled),
            input_border: p!(&cfg.input.border, &fallback.input.border),
            input_border_hover: p!(&cfg.input.border_hover, &fallback.input.border_hover),
            input_border_focused: p!(&cfg.input.border_focused, &fallback.input.border_focused),
            input_border_error: p!(&cfg.input.border_error, &fallback.input.border_error),
            input_placeholder: p!(&cfg.input.placeholder, &fallback.input.placeholder),
            input_selection: p!(&cfg.input.selection, &fallback.input.selection),
            command_overlay: p!(&cfg.command.overlay, &fallback.command.overlay),
            command_bg: p!(&cfg.command.bg, &fallback.command.bg),
            command_border: p!(&cfg.command.border, &fallback.command.border),
            command_item_hover: p!(&cfg.command.item_hover, &fallback.command.item_hover),
            command_item_active: p!(&cfg.command.item_active, &fallback.command.item_active),
            command_group_label: p!(&cfg.command.group_label, &fallback.command.group_label),
            term_fg: p!(&cfg.terminal.fg, &fallback.terminal.fg),
            term_bg: p!(&cfg.terminal.bg, &fallback.terminal.bg),
            term_cursor: p!(&cfg.terminal.cursor, &fallback.terminal.cursor),
            term_black: p!(&cfg.terminal.black, &fallback.terminal.black),
            term_red: p!(&cfg.terminal.red, &fallback.terminal.red),
            term_green: p!(&cfg.terminal.green, &fallback.terminal.green),
            term_yellow: p!(&cfg.terminal.yellow, &fallback.terminal.yellow),
            term_blue: p!(&cfg.terminal.blue, &fallback.terminal.blue),
            term_magenta: p!(&cfg.terminal.magenta, &fallback.terminal.magenta),
            term_cyan: p!(&cfg.terminal.cyan, &fallback.terminal.cyan),
            term_white: p!(&cfg.terminal.white, &fallback.terminal.white),
            term_bright_black: p!(&cfg.terminal.bright_black, &fallback.terminal.bright_black),
            term_bright_red: p!(&cfg.terminal.bright_red, &fallback.terminal.bright_red),
            term_bright_green: p!(&cfg.terminal.bright_green, &fallback.terminal.bright_green),
            term_bright_yellow: p!(
                &cfg.terminal.bright_yellow,
                &fallback.terminal.bright_yellow
            ),
            term_bright_blue: p!(&cfg.terminal.bright_blue, &fallback.terminal.bright_blue),
            term_bright_magenta: p!(
                &cfg.terminal.bright_magenta,
                &fallback.terminal.bright_magenta
            ),
            term_bright_cyan: p!(&cfg.terminal.bright_cyan, &fallback.terminal.bright_cyan),
            term_bright_white: p!(&cfg.terminal.bright_white, &fallback.terminal.bright_white),
            selection_bg: p!(&cfg.selection.bg, &fallback.selection.bg),
        }
    }
}

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
/// blue (the original bug behind the "阴间配色" report).
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

// ---------------------------------------------------------------------------
// snake_case accessors
//
// One per field. Render code calls `color::bg_base()` etc., and the call is
// just `*THEME.read()` + a field read — a handful of ns. We don't expose
// the `Theme` directly to call sites because the accessor form keeps the
// "always reflects the latest config" invariant local to this module.
// ---------------------------------------------------------------------------

macro_rules! accessors {
    ( $( $name:ident => $field:ident ),+ $(,)? ) => {
        $(
            pub fn $name() -> u32 {
                theme().$field
            }
        )+
    };
}

accessors!(
    bg_base => bg_base,
    bg_sidebar => bg_sidebar,
    bg_tab_bar => bg_tab_bar,
    border => border,
    surface_hover => surface_hover,
    surface_active => surface_active,
    text_primary => text_primary,
    text_muted => text_muted,
    tab_btn_bg => tab_btn_bg,
    tab_btn_bg_hover => tab_btn_bg_hover,
    tab_btn_bg_selected => tab_btn_bg_selected,
    tab_btn_bg_pressed => tab_btn_bg_pressed,
    tab_btn_bg_disabled => tab_btn_bg_disabled,
    tab_btn_border => tab_btn_border,
    tab_btn_text_disabled => tab_btn_text_disabled,
    btn_bg => btn_bg,
    btn_bg_hover => btn_bg_hover,
    btn_bg_selected => btn_bg_selected,
    btn_bg_pressed => btn_bg_pressed,
    btn_bg_disabled => btn_bg_disabled,
    btn_border => btn_border,
    btn_text_disabled => btn_text_disabled,
    btn_primary_bg => btn_primary_bg,
    btn_primary_bg_hover => btn_primary_bg_hover,
    btn_primary_bg_selected => btn_primary_bg_selected,
    btn_primary_bg_disabled => btn_primary_bg_disabled,
    btn_primary_border => btn_primary_border,
    btn_primary_text_disabled => btn_primary_text_disabled,
    btn_ghost_bg => btn_ghost_bg,
    btn_ghost_bg_hover => btn_ghost_bg_hover,
    btn_ghost_bg_selected => btn_ghost_bg_selected,
    btn_ghost_bg_disabled => btn_ghost_bg_disabled,
    btn_ghost_border => btn_ghost_border,
    btn_ghost_text_disabled => btn_ghost_text_disabled,
    input_bg => input_bg,
    input_bg_focused => input_bg_focused,
    input_bg_disabled => input_bg_disabled,
    input_text_disabled => input_text_disabled,
    input_border => input_border,
    input_border_hover => input_border_hover,
    input_border_focused => input_border_focused,
    input_border_error => input_border_error,
    input_placeholder => input_placeholder,
    input_selection => input_selection,
    command_overlay => command_overlay,
    command_bg => command_bg,
    command_border => command_border,
    command_item_hover => command_item_hover,
    command_item_active => command_item_active,
    command_group_label => command_group_label,
    term_fg => term_fg,
    term_bg => term_bg,
    term_cursor => term_cursor,
    term_black => term_black,
    term_red => term_red,
    term_green => term_green,
    term_yellow => term_yellow,
    term_blue => term_blue,
    term_magenta => term_magenta,
    term_cyan => term_cyan,
    term_white => term_white,
    term_bright_black => term_bright_black,
    term_bright_red => term_bright_red,
    term_bright_green => term_bright_green,
    term_bright_yellow => term_bright_yellow,
    term_bright_blue => term_bright_blue,
    term_bright_magenta => term_bright_magenta,
    term_bright_cyan => term_bright_cyan,
    term_bright_white => term_bright_white,
    selection_bg => selection_bg,
);
