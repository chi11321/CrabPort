//! Application configuration (`config.toml`).
//!
//! A single process-wide `CrabPortConfig` is exposed via the [`CONFIG`]
//! `LazyLock` — load-on-first-access, mutate through [`update`], and persist
//! to `{data_dir}/crabport/config.toml`.
//!
//! Why a `LazyLock` instead of a GPUI global? The settings window needs to
//! read/write config from contexts that may not have a `cx` handy (e.g. the
//! terminal pane reading its font size), and we want the same handle to be
//! reachable from `crabport-core` without introducing a circular dependency
//! on `gpui`. A `parking_lot::RwLock`-guarded `Arc` matches the
//! `Send + Sync` requirements of a static.
//!
//! # File layout
//!
//! ```text
//! {data_dir}/crabport/
//!   crabport.db       — SQLite database (hosts, credentials, ...)
//!   .key              — AES-256 encryption key
//!   config.toml       — this module's persisted config
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Sub-config structs
// ---------------------------------------------------------------------------

/// User-configurable appearance settings. Stored under `[appearance]` in
/// `config.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// Currently-active UI language code, e.g. "en" or "zh-CN". Mirrors
    /// the value passed to `rust_i18n::set_locale` in the binary crate.
    #[serde(default = "default_locale")]
    pub locale: String,

    /// Color theme. Every UI + terminal color is stored as a hex string
    /// (e.g. `"#1e1e2e"` or `"#RRGGBBAA"`) so users can hand-edit
    /// `config.toml`. Missing fields fall back to the modern-dark default.
    #[serde(default)]
    pub theme: ThemeConfig,

    /// Terminal font + size settings. Stored under `[appearance.terminal]`.
    #[serde(default)]
    pub terminal: TerminalConfig,

    /// Right-hand panel width in CSS pixels. Clamped at use sites into a
    /// sane range. Stored under `[appearance]` so it survives restarts.
    #[serde(default = "default_panel_width")]
    pub panel_width: f32,
}

fn default_panel_width() -> f32 {
    220.0
}

// ---------------------------------------------------------------------------
// Keybind config
// ---------------------------------------------------------------------------

/// User-configurable keyboard shortcuts. Stored under `[keybinds]` in
/// `config.toml` as a map of action-id → keystroke string (e.g.
/// `"toggle_command" = "cmd-k"`).
///
/// Action IDs are stable strings defined by the app's keybind catalog
/// (see `crabport-ui::keybinds`). Missing entries fall back to the
/// built-in defaults registered in `main.rs`.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct KeybindConfig {
    /// Map of action-id → GPUI keystroke string (e.g. "cmd-k", "ctrl-shift-c").
    /// An empty string disables the binding.
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

impl KeybindConfig {
    /// Get the keystroke for an action id, if present.
    pub fn get(&self, action_id: &str) -> Option<&str> {
        self.bindings.get(action_id).map(|s| s.as_str())
    }

    /// Set or update the keystroke for an action id.
    pub fn set(&mut self, action_id: &str, keystroke: &str) {
        self.bindings
            .insert(action_id.to_string(), keystroke.to_string());
    }
}

fn default_locale() -> String {
    "en".to_string()
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            theme: ThemeConfig::default(),
            terminal: TerminalConfig::default(),
            panel_width: default_panel_width(),
        }
    }
}

// ---------------------------------------------------------------------------
// TerminalConfig
// ---------------------------------------------------------------------------

/// Terminal font configuration. Stored under `[appearance.terminal]` in
/// `config.toml`.
///
/// `font_family` is the family name (e.g. `"Menlo"`); an empty string means
/// "use the platform default monospace". `font_size` is in CSS pixels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Monospace font family name. An empty value falls back to the
    /// platform-native default (`Menlo` on macOS, `Consolas` on Windows,
    /// `DejaVu Sans Mono` elsewhere) so a fresh install works out of the
    /// box without knowing font names.
    #[serde(default)]
    pub font_family: String,

    /// Font size in CSS pixels. Clamped into `[8.0, 32.0]` at use sites.
    #[serde(default = "default_terminal_font_size")]
    pub font_size: f32,
}

fn default_terminal_font_size() -> f32 {
    13.0
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_terminal_font_size(),
        }
    }
}

impl TerminalConfig {
    /// Resolve the effective font family, substituting the platform-native
    /// monospace default when the configured value is empty.
    pub fn effective_font_family(&self) -> &str {
        if self.font_family.is_empty() {
            default_terminal_font_family()
        } else {
            self.font_family.as_str()
        }
    }

    /// Clamp the configured font size into the supported range. Keeps
    /// hand-edited `config.toml` values from bricking the terminal.
    pub fn effective_font_size(&self) -> f32 {
        self.font_size.clamp(8.0, 32.0)
    }
}

/// Platform-native default monospace family. Matches the cell-width metrics
/// baked into the terminal renderer so a fresh install lines up cleanly.
pub fn default_terminal_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

// ---------------------------------------------------------------------------
// ThemeConfig
// ---------------------------------------------------------------------------

/// Color theme stored under `[appearance.theme]` in `config.toml`.
///
/// Every field is a hex string (`"#rrggbb"`, `"rrggbb"`, or `"#rrggbbaa"`
/// for colors that need an alpha channel) so the file stays diff-friendly
/// and editable by hand. The UI parses them into `u32` via
/// `crabport_ui::color::Theme::from_config`.
///
/// `Default` is the built-in "modern-dark" palette — a refined, slightly
/// cool neutral dark with an indigo accent. Other presets are available via
/// [`ThemeConfig::mocha`] / [`ThemeConfig::tokyo_night`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Preset name label (informational; does not affect rendering).
    #[serde(default = "ThemeConfig::default_name")]
    pub name: String,

    // -- Base --------------------------------------------------------------
    pub bg_base: String,
    pub bg_sidebar: String,
    pub bg_tab_bar: String,

    // -- Border ------------------------------------------------------------
    pub border: String,

    // -- Surface -----------------------------------------------------------
    pub surface_hover: String,
    pub surface_active: String,

    // -- Text --------------------------------------------------------------
    pub text_primary: String,
    pub text_muted: String,

    // -- Tab button (subtle, blends with sidebar/tabbar) ------------------
    pub tab_btn_bg: String,
    pub tab_btn_bg_hover: String,
    pub tab_btn_bg_selected: String,
    pub tab_btn_bg_pressed: String,
    pub tab_btn_bg_disabled: String,
    pub tab_btn_border: String,
    pub tab_btn_text_disabled: String,

    // -- Button (prominent) -----------------------------------------------
    pub btn_bg: String,
    pub btn_bg_hover: String,
    pub btn_bg_selected: String,
    pub btn_bg_pressed: String,
    pub btn_bg_disabled: String,
    pub btn_border: String,
    pub btn_text_disabled: String,

    // -- Button — primary (accent) ----------------------------------------
    pub btn_primary_bg: String,
    pub btn_primary_bg_hover: String,
    pub btn_primary_bg_selected: String,
    pub btn_primary_bg_disabled: String,
    pub btn_primary_border: String,
    pub btn_primary_text_disabled: String,

    // -- Button — ghost (transparent, icon-only friendly) -----------------
    pub btn_ghost_bg: String,
    pub btn_ghost_bg_hover: String,
    pub btn_ghost_bg_selected: String,
    pub btn_ghost_bg_disabled: String,
    pub btn_ghost_border: String,
    pub btn_ghost_text_disabled: String,

    // -- Input -------------------------------------------------------------
    pub input_bg: String,
    pub input_bg_focused: String,
    pub input_bg_disabled: String,
    pub input_text_disabled: String,
    pub input_border: String,
    pub input_border_hover: String,
    pub input_border_focused: String,
    pub input_border_error: String,
    pub input_placeholder: String,
    pub input_selection: String,

    // -- Command palette ---------------------------------------------------
    pub command_overlay: String,
    pub command_bg: String,
    pub command_border: String,
    pub command_item_hover: String,
    pub command_item_active: String,
    pub command_group_label: String,

    // -- Terminal ANSI palette --------------------------------------------
    pub term_fg: String,
    pub term_bg: String,
    pub term_cursor: String,
    pub term_black: String,
    pub term_red: String,
    pub term_green: String,
    pub term_yellow: String,
    pub term_blue: String,
    pub term_magenta: String,
    pub term_cyan: String,
    pub term_white: String,
    pub term_bright_black: String,
    pub term_bright_red: String,
    pub term_bright_green: String,
    pub term_bright_yellow: String,
    pub term_bright_blue: String,
    pub term_bright_magenta: String,
    pub term_bright_cyan: String,
    pub term_bright_white: String,
    pub selection_bg: String,
}

impl ThemeConfig {
    /// Built-in preset names, in dropdown order.
    pub const PRESETS: &'static [&'static str] = &["modern-dark", "mocha", "tokyo-night"];

    /// Human-readable label for a preset id (proper-noun theme names are
    /// intentionally left untranslated).
    pub fn preset_label(id: &str) -> &'static str {
        match id {
            "mocha" => "Catppuccin Mocha",
            "tokyo-night" => "Tokyo Night",
            _ => "Modern Dark",
        }
    }

    fn default_name() -> String {
        "modern-dark".to_string()
    }

    /// Return the preset with the given id, falling back to the default.
    pub fn preset(id: &str) -> Self {
        match id {
            "mocha" => Self::mocha(),
            "tokyo-night" => Self::tokyo_night(),
            _ => Self::modern_dark(),
        }
    }

    /// "Modern Dark" — the new default. A refined, slightly cool neutral
    /// dark with an indigo accent and well-tuned neutrals. Higher contrast
    /// and less purple cast than the legacy Mocha palette.
    pub fn modern_dark() -> Self {
        Self {
            name: "modern-dark".into(),
            // Base
            bg_base: "#14161c".into(),
            bg_sidebar: "#0f1116".into(),
            bg_tab_bar: "#0f1116".into(),
            // Border
            border: "#23262f".into(),
            // Surface
            surface_hover: "#1c1f27".into(),
            surface_active: "#262a34".into(),
            // Text
            text_primary: "#e6e9ef".into(),
            text_muted: "#8b90a0".into(),
            // Tab button
            tab_btn_bg: "#0f1116".into(),
            tab_btn_bg_hover: "#1c1f27".into(),
            tab_btn_bg_selected: "#262a34".into(),
            tab_btn_bg_pressed: "#2e333f".into(),
            tab_btn_bg_disabled: "#0a0c10".into(),
            tab_btn_border: "#23262f".into(),
            tab_btn_text_disabled: "#2e333f".into(),
            // Button
            btn_bg: "#262a34".into(),
            btn_bg_hover: "#2e333f".into(),
            btn_bg_selected: "#363b48".into(),
            btn_bg_pressed: "#3f4452".into(),
            btn_bg_disabled: "#14161c".into(),
            btn_border: "#2e333f".into(),
            btn_text_disabled: "#6b7080".into(),
            // Button — primary (indigo)
            btn_primary_bg: "#6366f1".into(),
            btn_primary_bg_hover: "#4f46e5".into(),
            btn_primary_bg_selected: "#4338ca".into(),
            btn_primary_bg_disabled: "#312e81".into(),
            btn_primary_border: "#6366f1".into(),
            btn_primary_text_disabled: "#a5b4fc".into(),
            // Button — ghost
            btn_ghost_bg: "#00000000".into(),
            btn_ghost_bg_hover: "#2e333fff".into(),
            btn_ghost_bg_selected: "#262a34ff".into(),
            btn_ghost_bg_disabled: "#00000000".into(),
            btn_ghost_border: "#00000000".into(),
            btn_ghost_text_disabled: "#6b7080ff".into(),
            // Input
            input_bg: "#0f1116".into(),
            input_bg_focused: "#14161c".into(),
            input_bg_disabled: "#0a0c10".into(),
            input_text_disabled: "#2e333f".into(),
            input_border: "#23262f".into(),
            input_border_hover: "#2e333f".into(),
            input_border_focused: "#818cf8".into(),
            input_border_error: "#f87171".into(),
            input_placeholder: "#6b7080".into(),
            input_selection: "#818cf833".into(),
            // Command
            command_overlay: "#00000050".into(),
            command_bg: "#14161c".into(),
            command_border: "#23262f".into(),
            command_item_hover: "#1c1f27".into(),
            command_item_active: "#262a34".into(),
            command_group_label: "#6b7080".into(),
            // Terminal ANSI
            term_fg: "#e6e9ef".into(),
            term_bg: "#14161c".into(),
            term_cursor: "#c8cce4".into(),
            term_black: "#2e333f".into(),
            term_red: "#f87171".into(),
            term_green: "#4ade80".into(),
            term_yellow: "#facc15".into(),
            term_blue: "#818cf8".into(),
            term_magenta: "#e879f9".into(),
            term_cyan: "#22d3ee".into(),
            term_white: "#c1c5d0".into(),
            term_bright_black: "#6b7080".into(),
            term_bright_red: "#f87171".into(),
            term_bright_green: "#4ade80".into(),
            term_bright_yellow: "#facc15".into(),
            term_bright_blue: "#818cf8".into(),
            term_bright_magenta: "#e879f9".into(),
            term_bright_cyan: "#22d3ee".into(),
            term_bright_white: "#e6e9ef".into(),
            selection_bg: "#6b7080".into(),
        }
    }

    /// "Catppuccin Mocha" — the legacy palette, kept for continuity.
    pub fn mocha() -> Self {
        Self {
            name: "mocha".into(),
            bg_base: "#1e1e2e".into(),
            bg_sidebar: "#181825".into(),
            bg_tab_bar: "#181825".into(),
            border: "#313244".into(),
            surface_hover: "#24273a".into(),
            surface_active: "#313244".into(),
            text_primary: "#cdd6f4".into(),
            text_muted: "#585b70".into(),
            tab_btn_bg: "#181825".into(),
            tab_btn_bg_hover: "#24273a".into(),
            tab_btn_bg_selected: "#313244".into(),
            tab_btn_bg_pressed: "#45475a".into(),
            tab_btn_bg_disabled: "#11111b".into(),
            tab_btn_border: "#313244".into(),
            tab_btn_text_disabled: "#45475a".into(),
            btn_bg: "#313244".into(),
            btn_bg_hover: "#45475a".into(),
            btn_bg_selected: "#585b70".into(),
            btn_bg_pressed: "#6c7086".into(),
            btn_bg_disabled: "#1e1e2e".into(),
            btn_border: "#45475a".into(),
            btn_text_disabled: "#585b70".into(),
            btn_primary_bg: "#3b82f6".into(),
            btn_primary_bg_hover: "#2563eb".into(),
            btn_primary_bg_selected: "#1d4ed8".into(),
            btn_primary_bg_disabled: "#1e3a5f".into(),
            btn_primary_border: "#3b82f6".into(),
            btn_primary_text_disabled: "#93c5fd".into(),
            btn_ghost_bg: "#00000000".into(),
            btn_ghost_bg_hover: "#45475aff".into(),
            btn_ghost_bg_selected: "#313244ff".into(),
            btn_ghost_bg_disabled: "#00000000".into(),
            btn_ghost_border: "#00000000".into(),
            btn_ghost_text_disabled: "#585b70ff".into(),
            input_bg: "#181825".into(),
            input_bg_focused: "#1e1e2e".into(),
            input_bg_disabled: "#11111b".into(),
            input_text_disabled: "#45475a".into(),
            input_border: "#313244".into(),
            input_border_hover: "#45475a".into(),
            input_border_focused: "#89b4fa".into(),
            input_border_error: "#ef4444".into(),
            input_placeholder: "#585b70".into(),
            input_selection: "#89b4fa33".into(),
            command_overlay: "#00000050".into(),
            command_bg: "#1e1e2e".into(),
            command_border: "#313244".into(),
            command_item_hover: "#24273a".into(),
            command_item_active: "#313244".into(),
            command_group_label: "#585b70".into(),
            term_fg: "#cdd6f4".into(),
            term_bg: "#1e1e2e".into(),
            term_cursor: "#f5e0dc".into(),
            term_black: "#45475a".into(),
            term_red: "#f38ba8".into(),
            term_green: "#a6e3a1".into(),
            term_yellow: "#f9e2af".into(),
            term_blue: "#89b4fa".into(),
            term_magenta: "#f5c2e7".into(),
            term_cyan: "#94e2d5".into(),
            term_white: "#bac2de".into(),
            term_bright_black: "#585b70".into(),
            term_bright_red: "#f38ba8".into(),
            term_bright_green: "#a6e3a1".into(),
            term_bright_yellow: "#f9e2af".into(),
            term_bright_blue: "#89b4fa".into(),
            term_bright_magenta: "#f5c2e7".into(),
            term_bright_cyan: "#94e2d5".into(),
            term_bright_white: "#a6adc8".into(),
            selection_bg: "#585b70".into(),
        }
    }

    /// "Tokyo Night" — a popular cool-toned blue/indigo dark palette.
    pub fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night".into(),
            bg_base: "#1a1b26".into(),
            bg_sidebar: "#16161e".into(),
            bg_tab_bar: "#16161e".into(),
            border: "#2a2b3d".into(),
            surface_hover: "#1f2335".into(),
            surface_active: "#292e42".into(),
            text_primary: "#c0caf5".into(),
            text_muted: "#565f89".into(),
            tab_btn_bg: "#16161e".into(),
            tab_btn_bg_hover: "#1f2335".into(),
            tab_btn_bg_selected: "#292e42".into(),
            tab_btn_bg_pressed: "#3b4261".into(),
            tab_btn_bg_disabled: "#101014".into(),
            tab_btn_border: "#2a2b3d".into(),
            tab_btn_text_disabled: "#3b4261".into(),
            btn_bg: "#292e42".into(),
            btn_bg_hover: "#3b4261".into(),
            btn_bg_selected: "#414868".into(),
            btn_bg_pressed: "#4c5375".into(),
            btn_bg_disabled: "#1a1b26".into(),
            btn_border: "#3b4261".into(),
            btn_text_disabled: "#565f89".into(),
            btn_primary_bg: "#7aa2f7".into(),
            btn_primary_bg_hover: "#89b4fa".into(),
            btn_primary_bg_selected: "#6183bb".into(),
            btn_primary_bg_disabled: "#2e3a5f".into(),
            btn_primary_border: "#7aa2f7".into(),
            btn_primary_text_disabled: "#b4c5e8".into(),
            btn_ghost_bg: "#00000000".into(),
            btn_ghost_bg_hover: "#3b4261ff".into(),
            btn_ghost_bg_selected: "#292e42ff".into(),
            btn_ghost_bg_disabled: "#00000000".into(),
            btn_ghost_border: "#00000000".into(),
            btn_ghost_text_disabled: "#565f89ff".into(),
            input_bg: "#16161e".into(),
            input_bg_focused: "#1a1b26".into(),
            input_bg_disabled: "#101014".into(),
            input_text_disabled: "#3b4261".into(),
            input_border: "#2a2b3d".into(),
            input_border_hover: "#3b4261".into(),
            input_border_focused: "#7aa2f7".into(),
            input_border_error: "#f7768e".into(),
            input_placeholder: "#565f89".into(),
            input_selection: "#7aa2f733".into(),
            command_overlay: "#00000050".into(),
            command_bg: "#1a1b26".into(),
            command_border: "#2a2b3d".into(),
            command_item_hover: "#1f2335".into(),
            command_item_active: "#292e42".into(),
            command_group_label: "#565f89".into(),
            term_fg: "#c0caf5".into(),
            term_bg: "#1a1b26".into(),
            term_cursor: "#c0caf5".into(),
            term_black: "#414868".into(),
            term_red: "#f7768e".into(),
            term_green: "#9ece6a".into(),
            term_yellow: "#e0af68".into(),
            term_blue: "#7aa2f7".into(),
            term_magenta: "#bb9af7".into(),
            term_cyan: "#7dcfff".into(),
            term_white: "#a9b1d6".into(),
            term_bright_black: "#565f89".into(),
            term_bright_red: "#f7768e".into(),
            term_bright_green: "#9ece6a".into(),
            term_bright_yellow: "#e0af68".into(),
            term_bright_blue: "#7aa2f7".into(),
            term_bright_magenta: "#bb9af7".into(),
            term_bright_cyan: "#7dcfff".into(),
            term_bright_white: "#c0caf5".into(),
            selection_bg: "#33467c".into(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self::modern_dark()
    }
}

/// Top-level config root, serialized to `config.toml` and reachable via the
/// [`CONFIG`] `LazyLock`.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CrabPortConfig {
    #[serde(default)]
    pub appearance: AppearanceConfig,

    /// User-configurable keyboard shortcuts. Stored under `[keybinds]`.
    #[serde(default)]
    pub keybinds: KeybindConfig,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConfigError {
    Io(String),
    Parse(String),
    Serialize(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "IO: {e}"),
            ConfigError::Parse(e) => write!(f, "Parse: {e}"),
            ConfigError::Serialize(e) => write!(f, "Serialize: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e.to_string())
    }
}

impl From<toml::ser::Error> for ConfigError {
    fn from(e: toml::ser::Error) -> Self {
        ConfigError::Serialize(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// LazyLock global
// ---------------------------------------------------------------------------

/// Process-wide configuration handle. Initialized on first access from the
/// on-disk `config.toml` (or defaults if the file does not exist yet).
pub static CONFIG: LazyLock<Arc<RwLock<CrabPortConfig>>> =
    LazyLock::new(|| Arc::new(RwLock::new(load().unwrap_or_default())));

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Path to the `config.toml` file inside the CrabPort data directory.
/// Re-uses the same `dirs::data_dir()` root as the SQLite store so config and
/// credentials live next to each other.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let base =
        dirs::data_dir().ok_or_else(|| ConfigError::Io("cannot determine data dir".into()))?;
    Ok(base.join("crabport").join("config.toml"))
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Read `config.toml` from disk and deserialize it. Returns `Ok(defaults)`
/// when the file does not exist yet (fresh install) so callers don't have to
/// distinguish "missing" from "present".
pub fn load() -> Result<CrabPortConfig, ConfigError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(CrabPortConfig::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let cfg: CrabPortConfig = toml::from_str(&text)?;
    Ok(cfg)
}

/// Serialize and atomically write `cfg` to `config.toml`. Creates the parent
/// directory if needed. Atomicity is provided by writing to a `.tmp` file and
/// renaming — a crash mid-write won't corrupt the existing config.
pub fn save(cfg: &CrabPortConfig) -> Result<(), ConfigError> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ConfigError::Io(e.to_string()))?;
    }
    let text = toml::to_string_pretty(cfg)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| ConfigError::Io(e.to_string()))?;
    fs::rename(&tmp, &path).map_err(|e| ConfigError::Io(e.to_string()))?;
    Ok(())
}

/// Mutate the live config inside the [`CONFIG`] lock, then persist it to
/// disk. Use this from the UI: the closure sees a `&mut CrabPortConfig`, and
/// the lock is held only for the duration of the mutation.
///
/// Returns the *post-mutation* snapshot so callers can react to the new
/// values (e.g. apply the new locale).
pub fn update<R>(f: impl FnOnce(&mut CrabPortConfig) -> R) -> Result<R, ConfigError> {
    let mut guard = CONFIG.write();
    let ret = f(&mut guard);
    save(&guard)?;
    Ok(ret)
}

/// Convenience: take a read lock and clone the current config snapshot.
pub fn snapshot() -> CrabPortConfig {
    CONFIG.read().clone()
}
