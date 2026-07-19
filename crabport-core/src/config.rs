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

    /// Which view to open at app launch. Stored under `[appearance.startup]`
    /// so it survives restarts alongside other general UI prefs.
    #[serde(default)]
    pub startup: StartupConfig,
}

fn default_panel_width() -> f32 {
    220.0
}

// ---------------------------------------------------------------------------
// Startup config
// ---------------------------------------------------------------------------

/// User-configurable launch behavior. Stored under `[appearance.startup]`
/// in `config.toml`.
///
/// `Home`, `Sftp`, and `LocalTerminal` resolve to the corresponding built-in
/// tab kinds; `Session(id)` opens the saved host with that id. A stale
/// `Session` id (host deleted from the store) falls back to `Home` at launch.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StartupConfig {
    /// Where to land when the app starts. `Home` by default so a fresh
    /// install behaves predictably.
    #[serde(default)]
    pub page: StartupPage,
}

/// The launch target. Serialized as a single-tagged string
/// (`"home"`, `"sftp"`, `"local_terminal"`, `"session:<id>"`)
/// so hand-editing `config.toml` stays readable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartupPage {
    /// Land on the Home (sessions) tab.
    #[default]
    Home,
    /// Land on the SFTP tab (id=1).
    Sftp,
    /// Open a new local terminal tab.
    LocalTerminal,
    /// Reconnect to a saved session by host id. If the host no longer
    /// exists at launch time, the app falls back to `Home`.
    Session(i64),
}

impl StartupPage {
    /// Stable string id used as the dropdown item value. Round-trips via
    /// [`StartupPage::from_id`].
    pub fn to_id(&self) -> String {
        match self {
            StartupPage::Home => "home".to_string(),
            StartupPage::Sftp => "sftp".to_string(),
            StartupPage::LocalTerminal => "local_terminal".to_string(),
            StartupPage::Session(id) => format!("session:{id}"),
        }
    }

    /// Parse a string id back into a [`StartupPage`]. Unknown / malformed
    /// values fall back to `Home` so a corrupted `config.toml` can never
    /// brick launch.
    pub fn from_id(s: &str) -> Self {
        if s == "home" {
            return StartupPage::Home;
        }
        if s == "sftp" {
            return StartupPage::Sftp;
        }
        if s == "local_terminal" {
            return StartupPage::LocalTerminal;
        }
        if let Some(rest) = s.strip_prefix("session:") {
            if let Ok(id) = rest.parse::<i64>() {
                return StartupPage::Session(id);
            }
        }
        StartupPage::Home
    }
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

/// Default UI locale used when `config.toml` doesn't pin one yet
/// (fresh install, or the field was removed). Resolves to the current OS
/// locale when it's a Chinese variant, otherwise falls back to `en` —
/// the two locales CrabPort ships translations for.
///
/// This only runs for *missing* `locale` fields thanks to the
/// `#[serde(default = "default_locale")]` attribute, so an explicit user
/// choice in Settings (which is persisted immediately) always wins, and
/// existing `config.toml` files that already pin `locale = "en"` are left
/// untouched.
fn default_locale() -> String {
    let sys = sys_locale::get_locale().unwrap_or_default();
    if sys.to_lowercase().starts_with("zh") {
        "zh-CN".to_string()
    } else {
        "en".to_string()
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            theme: ThemeConfig::default(),
            terminal: TerminalConfig::default(),
            panel_width: default_panel_width(),
            startup: StartupConfig::default(),
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

    /// Per-slot visibility for the bottom toolbar, stored under
    /// `[appearance.terminal.toolbar]`. Each field defaults to `true` so a
    /// fresh install shows every available chip; the user toggles them
    /// via the gear context menu in the toolbar itself. Fields that don't
    /// exist in the struct (e.g. ones added in a later version) just get
    /// the `#[serde(default)]` value, so config round-trips cleanly across
    /// versions.
    #[serde(default)]
    pub toolbar: ToolbarVisibilityConfig,
}

fn default_terminal_font_size() -> f32 {
    13.0
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_terminal_font_size(),
            toolbar: ToolbarVisibilityConfig::default(),
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

// ---------------------------------------------------------------------------
// ToolbarVisibilityConfig
// ---------------------------------------------------------------------------

/// Per-slot visibility for the bottom toolbar. Stored under
/// `[appearance.terminal.toolbar]` in `config.toml`. Each boolean toggles
/// one toolbar chip on (true) or off (false). The gear context menu in the
/// toolbar flips these live; the change is persisted immediately.
///
/// Every field defaults to `true` so a fresh install shows the full toolbar.
/// Fields not in this struct (added in later versions) silently fall back to
/// `true` thanks to `#[serde(default)]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolbarVisibilityConfig {
    #[serde(default = "default_true")]
    pub latency: bool,
    #[serde(default = "default_true")]
    pub cpu: bool,
    #[serde(default = "default_true")]
    pub disk: bool,
    #[serde(default = "default_true")]
    pub memory: bool,
    #[serde(default = "default_true")]
    pub network: bool,
    /// SFTP transfer progress chip (right-aligned in the terminal toolbar).
    #[serde(default = "default_true")]
    pub sftp_progress: bool,
    /// "SFTP transfer history" toggle button in the SFTP tab toolbar.
    /// Defaults to `false` because the panel is opt-in — surfacing it by
    /// default would draw the user's attention to a feature they may not
    /// know about yet.
    #[serde(default = "default_false")]
    pub sftp_history: bool,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for ToolbarVisibilityConfig {
    fn default() -> Self {
        Self {
            latency: true,
            cpu: true,
            disk: true,
            memory: true,
            network: true,
            sftp_progress: true,
            sftp_history: false,
        }
    }
}

impl ToolbarVisibilityConfig {
    /// Toggle the visibility for the slot identified by `id`. The `id`
    /// strings are the same `&'static str` discriminators used by
    /// [`crate::layouts::toolbar::ToolbarSlot::id`]. An unknown id is a
    /// no-op so the gear menu doesn't panic if a stale slot id is passed
    /// from an older build.
    pub fn toggle(&mut self, id: &str) {
        match id {
            "latency" => self.latency = !self.latency,
            "cpu" => self.cpu = !self.cpu,
            "disk" => self.disk = !self.disk,
            "memory" => self.memory = !self.memory,
            "network" => self.network = !self.network,
            "sftp_progress" => self.sftp_progress = !self.sftp_progress,
            "sftp_history" => self.sftp_history = !self.sftp_history,
            _ => {}
        }
    }

    /// Read the visibility for the slot identified by `id`. Unknown ids
    /// default to `true` so a slot that the config doesn't yet know about
    /// still shows up (matching the `#[serde(default)]` semantics on the
    /// struct fields).
    pub fn get(&self, id: &str) -> bool {
        match id {
            "latency" => self.latency,
            "cpu" => self.cpu,
            "disk" => self.disk,
            "memory" => self.memory,
            "network" => self.network,
            "sftp_progress" => self.sftp_progress,
            "sftp_history" => self.sftp_history,
            _ => true,
        }
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
/// The theme is split into nested sub-tables mirroring the UI's logical
/// color groups (`[theme.base]`, `[theme.surface]`, `[theme.text]`,
/// `[theme.button]`, `[theme.button_primary]`, `[theme.button_ghost]`,
/// `[theme.tab_button]`, `[theme.input]`, `[theme.command]`,
/// `[theme.terminal]`, `[theme.selection]`) — same two-level structure the
/// i18n files use. This keeps a hand-edited `config.toml` scannable and lets
/// a user override just one group (e.g. `[theme.terminal]`) while the rest
/// fall back to the modern-dark defaults.
///
/// Every leaf color is a hex string (`"#rrggbb"`, `"rrggbb"`, or
/// `"#rrggbbaa"` for colors that need an alpha channel) so the file stays
/// diff-friendly. The UI parses them into `u32` via
/// `crabport_ui::color::Theme::from_config`, which falls back to the
/// modern-dark value for any empty / malformed string — so a `#[serde(default)]`
/// that yields an empty `String` is always safe (no per-field default fns
/// needed).
///
/// `Default` is the built-in "modern-dark" palette — a refined, slightly
/// cool neutral dark with an indigo accent. Other presets are available via
/// [`ThemeConfig::mocha`] / [`ThemeConfig::tokyo_night`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Preset name label — also the theme id used by the catalog and the
    /// Settings dropdown. Informational for rendering, but round-trips
    /// through `config.toml` so the selected theme survives restarts.
    #[serde(default = "ThemeConfig::default_name")]
    pub name: String,

    /// Base window backgrounds (root, sidebar, tab bar).
    #[serde(default)]
    pub base: ThemeBase,
    /// Single shared border color used across most dividers.
    #[serde(default)]
    pub border: ThemeBorder,
    /// Surface fills for hover / active states.
    #[serde(default)]
    pub surface: ThemeSurface,
    /// Primary / muted text colors.
    #[serde(default)]
    pub text: ThemeText,
    /// Default (non-primary, non-ghost) button colors.
    #[serde(default)]
    pub button: ThemeButton,
    /// Primary (accent) button colors — the prominent CTA style.
    #[serde(default)]
    pub button_primary: ThemeButton,
    /// Ghost (transparent / icon-only) button colors.
    #[serde(default)]
    pub button_ghost: ThemeButton,
    /// Tab button colors (the sidebar/tab-bar pill buttons).
    #[serde(default)]
    pub tab_button: ThemeButton,
    /// Input field colors (text inputs, dropdowns, textareas).
    #[serde(default)]
    pub input: ThemeInput,
    /// Command palette overlay + items.
    #[serde(default)]
    pub command: ThemeCommand,
    /// Terminal ANSI 16-color palette + fg/bg/cursor.
    #[serde(default)]
    pub terminal: ThemeTerminal,
    /// Text selection background.
    #[serde(default)]
    pub selection: ThemeSelection,
}

/// Define a theme sub-group struct.
///
/// Generates a `#[derive(Clone, Debug, Serialize, Deserialize, Default)]`
/// struct with one `pub` `String` field per listed name, each annotated
/// `#[serde(default)]` (empty string). An empty leaf is harmless because
/// `Theme::from_config` falls back to the modern-dark value for any string
/// that fails to parse as hex — so we don't need 50 per-field `default =
/// "..."` functions, just one `Default` derive per group.
macro_rules! theme_group {
    ($name:ident; $($field:ident),+ $(,)?) => {
        #[derive(Clone, Debug, Serialize, Deserialize, Default)]
        pub struct $name {
            $(
                #[serde(default)]
                pub $field: String,
            )+
        }
    };
}

theme_group!(ThemeBase; bg_base, bg_sidebar, bg_tab_bar);
theme_group!(ThemeBorder; border);
theme_group!(ThemeSurface; surface_hover, surface_active);
theme_group!(ThemeText; text_primary, text_muted);
theme_group!(ThemeButton; bg, bg_hover, bg_selected, bg_pressed, bg_disabled, border, text_disabled);
theme_group!(ThemeInput; bg, bg_focused, bg_disabled, text_disabled, border, border_hover, border_focused, border_error, placeholder, selection);
theme_group!(ThemeCommand; overlay, bg, border, item_hover, item_active, group_label);
theme_group!(ThemeTerminal; fg, bg, cursor, black, red, green, yellow, blue, magenta, cyan, white, bright_black, bright_red, bright_green, bright_yellow, bright_blue, bright_magenta, bright_cyan, bright_white);
theme_group!(ThemeSelection; bg);

/// Construct a theme sub-group from `field: "#hex"` pairs. Unlisted fields
/// default to the empty string (via `..Default::default()`), which
/// `Theme::from_config` substitutes with the modern-dark value — so a preset
/// can omit fields it doesn't care about and inherit the default. Every
/// preset below lists all fields anyway, so this is just a shorthand to drop
/// the repeated `"#xxx".into()` boilerplate.
macro_rules! tc {
    ($group:ident; $($field:ident: $hex:literal),+ $(,)?) => {
        $group {
            $(
                $field: $hex.into(),
            )+
            ..Default::default()
        }
    };
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
            base: tc!(ThemeBase;
                bg_base: "#14161c", bg_sidebar: "#0f1116", bg_tab_bar: "#0f1116"),
            border: tc!(ThemeBorder; border: "#23262f"),
            surface: tc!(ThemeSurface; surface_hover: "#1c1f27", surface_active: "#262a34"),
            text: tc!(ThemeText; text_primary: "#e6e9ef", text_muted: "#8b90a0"),
            button: tc!(ThemeButton;
                bg: "#262a34", bg_hover: "#2e333f", bg_selected: "#363b48",
                bg_pressed: "#3f4452", bg_disabled: "#14161c",
                border: "#2e333f", text_disabled: "#6b7080"),
            button_primary: tc!(ThemeButton;
                bg: "#6366f1", bg_hover: "#4f46e5", bg_selected: "#4338ca",
                bg_disabled: "#312e81", border: "#6366f1", text_disabled: "#a5b4fc"),
            button_ghost: tc!(ThemeButton;
                bg: "#00000000", bg_hover: "#2e333fff", bg_selected: "#262a34ff",
                bg_disabled: "#00000000", border: "#00000000", text_disabled: "#6b7080ff"),
            tab_button: tc!(ThemeButton;
                bg: "#0f1116", bg_hover: "#1c1f27", bg_selected: "#262a34",
                bg_pressed: "#2e333f", bg_disabled: "#0a0c10",
                border: "#23262f", text_disabled: "#2e333f"),
            input: tc!(ThemeInput;
                bg: "#0f1116", bg_focused: "#14161c", bg_disabled: "#0a0c10",
                text_disabled: "#2e333f", border: "#23262f", border_hover: "#2e333f",
                border_focused: "#818cf8", border_error: "#f87171",
                placeholder: "#6b7080", selection: "#818cf833"),
            command: tc!(ThemeCommand;
                overlay: "#00000050", bg: "#14161c", border: "#23262f",
                item_hover: "#1c1f27", item_active: "#262a34", group_label: "#6b7080"),
            terminal: tc!(ThemeTerminal;
                fg: "#e6e9ef", bg: "#14161c", cursor: "#c8cce4",
                black: "#2e333f", red: "#f87171", green: "#4ade80", yellow: "#facc15",
                blue: "#818cf8", magenta: "#e879f9", cyan: "#22d3ee", white: "#c1c5d0",
                bright_black: "#6b7080", bright_red: "#f87171", bright_green: "#4ade80",
                bright_yellow: "#facc15", bright_blue: "#818cf8", bright_magenta: "#e879f9",
                bright_cyan: "#22d3ee", bright_white: "#e6e9ef"),
            selection: tc!(ThemeSelection; bg: "#6b7080"),
        }
    }

    /// "Catppuccin Mocha" — the legacy palette, kept for continuity.
    pub fn mocha() -> Self {
        Self {
            name: "mocha".into(),
            base: tc!(ThemeBase;
                bg_base: "#1e1e2e", bg_sidebar: "#181825", bg_tab_bar: "#181825"),
            border: tc!(ThemeBorder; border: "#313244"),
            surface: tc!(ThemeSurface; surface_hover: "#24273a", surface_active: "#313244"),
            text: tc!(ThemeText; text_primary: "#cdd6f4", text_muted: "#585b70"),
            button: tc!(ThemeButton;
                bg: "#313244", bg_hover: "#45475a", bg_selected: "#585b70",
                bg_pressed: "#6c7086", bg_disabled: "#1e1e2e",
                border: "#45475a", text_disabled: "#585b70"),
            button_primary: tc!(ThemeButton;
                bg: "#3b82f6", bg_hover: "#2563eb", bg_selected: "#1d4ed8",
                bg_disabled: "#1e3a5f", border: "#3b82f6", text_disabled: "#93c5fd"),
            button_ghost: tc!(ThemeButton;
                bg: "#00000000", bg_hover: "#45475aff", bg_selected: "#313244ff",
                bg_disabled: "#00000000", border: "#00000000", text_disabled: "#585b70ff"),
            tab_button: tc!(ThemeButton;
                bg: "#181825", bg_hover: "#24273a", bg_selected: "#313244",
                bg_pressed: "#45475a", bg_disabled: "#11111b",
                border: "#313244", text_disabled: "#45475a"),
            input: tc!(ThemeInput;
                bg: "#181825", bg_focused: "#1e1e2e", bg_disabled: "#11111b",
                text_disabled: "#45475a", border: "#313244", border_hover: "#45475a",
                border_focused: "#89b4fa", border_error: "#ef4444",
                placeholder: "#585b70", selection: "#89b4fa33"),
            command: tc!(ThemeCommand;
                overlay: "#00000050", bg: "#1e1e2e", border: "#313244",
                item_hover: "#24273a", item_active: "#313244", group_label: "#585b70"),
            terminal: tc!(ThemeTerminal;
                fg: "#cdd6f4", bg: "#1e1e2e", cursor: "#f5e0dc",
                black: "#45475a", red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af",
                blue: "#89b4fa", magenta: "#f5c2e7", cyan: "#94e2d5", white: "#bac2de",
                bright_black: "#585b70", bright_red: "#f38ba8", bright_green: "#a6e3a1",
                bright_yellow: "#f9e2af", bright_blue: "#89b4fa", bright_magenta: "#f5c2e7",
                bright_cyan: "#94e2d5", bright_white: "#a6adc8"),
            selection: tc!(ThemeSelection; bg: "#585b70"),
        }
    }

    /// "Tokyo Night" — a popular cool-toned blue/indigo dark palette.
    pub fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night".into(),
            base: tc!(ThemeBase;
                bg_base: "#1a1b26", bg_sidebar: "#16161e", bg_tab_bar: "#16161e"),
            border: tc!(ThemeBorder; border: "#2a2b3d"),
            surface: tc!(ThemeSurface; surface_hover: "#1f2335", surface_active: "#292e42"),
            text: tc!(ThemeText; text_primary: "#c0caf5", text_muted: "#565f89"),
            button: tc!(ThemeButton;
                bg: "#292e42", bg_hover: "#3b4261", bg_selected: "#414868",
                bg_pressed: "#4c5375", bg_disabled: "#1a1b26",
                border: "#3b4261", text_disabled: "#565f89"),
            button_primary: tc!(ThemeButton;
                bg: "#7aa2f7", bg_hover: "#89b4fa", bg_selected: "#6183bb",
                bg_disabled: "#2e3a5f", border: "#7aa2f7", text_disabled: "#b4c5e8"),
            button_ghost: tc!(ThemeButton;
                bg: "#00000000", bg_hover: "#3b4261ff", bg_selected: "#292e42ff",
                bg_disabled: "#00000000", border: "#00000000", text_disabled: "#565f89ff"),
            tab_button: tc!(ThemeButton;
                bg: "#16161e", bg_hover: "#1f2335", bg_selected: "#292e42",
                bg_pressed: "#3b4261", bg_disabled: "#101014",
                border: "#2a2b3d", text_disabled: "#3b4261"),
            input: tc!(ThemeInput;
                bg: "#16161e", bg_focused: "#1a1b26", bg_disabled: "#101014",
                text_disabled: "#3b4261", border: "#2a2b3d", border_hover: "#3b4261",
                border_focused: "#7aa2f7", border_error: "#f7768e",
                placeholder: "#565f89", selection: "#7aa2f733"),
            command: tc!(ThemeCommand;
                overlay: "#00000050", bg: "#1a1b26", border: "#2a2b3d",
                item_hover: "#1f2335", item_active: "#292e42", group_label: "#565f89"),
            terminal: tc!(ThemeTerminal;
                fg: "#c0caf5", bg: "#1a1b26", cursor: "#c0caf5",
                black: "#414868", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68",
                blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6",
                bright_black: "#565f89", bright_red: "#f7768e", bright_green: "#9ece6a",
                bright_yellow: "#e0af68", bright_blue: "#7aa2f7", bright_magenta: "#bb9af7",
                bright_cyan: "#7dcfff", bright_white: "#c0caf5"),
            selection: tc!(ThemeSelection; bg: "#33467c"),
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
pub static CONFIG: LazyLock<Arc<RwLock<CrabPortConfig>>> = LazyLock::new(|| {
    match load() {
        Ok(cfg) => Arc::new(RwLock::new(cfg)),
        Err(e) => {
            // Don't panic — the app can still run on defaults. Log so the
            // user has a chance to notice a corrupted config.toml.
            tracing::warn!("config: failed to load config.toml ({e}) — using defaults");
            Arc::new(RwLock::new(CrabPortConfig::default()))
        }
    }
});

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A nested `[theme.terminal]` table parses into the matching sub-struct,
    /// and missing sub-tables fall back to empty strings (which `from_config`
    /// later substitutes with the modern-dark value).
    #[test]
    fn nested_theme_partial_parses() {
        let toml = r##"
name = "my-theme"

[terminal]
fg = "#abcdef"
bg = "#111111"
"##;
        let cfg: ThemeConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.name, "my-theme");
        assert_eq!(cfg.terminal.fg, "#abcdef");
        assert_eq!(cfg.terminal.bg, "#111111");
        // Missing group → empty default (from_config falls back to modern-dark).
        assert_eq!(cfg.base.bg_base, "");
        assert_eq!(cfg.button.bg, "");
        assert_eq!(cfg.selection.bg, "");
    }

    /// Every built-in preset round-trips through TOML serialize → parse
    /// without dropping a field. Catches typos in the `tc!` macro calls and
    /// serde attribute regressions.
    #[test]
    fn presets_roundtrip() {
        for preset in [
            ThemeConfig::modern_dark(),
            ThemeConfig::mocha(),
            ThemeConfig::tokyo_night(),
        ] {
            let text = toml::to_string_pretty(&preset).unwrap();
            let back: ThemeConfig = toml::from_str(&text).unwrap();
            assert_eq!(preset.name, back.name);
            assert_eq!(preset.base.bg_base, back.base.bg_base);
            assert_eq!(preset.terminal.bright_white, back.terminal.bright_white);
            assert_eq!(preset.selection.bg, back.selection.bg);
            assert_eq!(preset.button_primary.bg, back.button_primary.bg);
            assert_eq!(preset.tab_button.border, back.tab_button.border);
            assert_eq!(preset.input.selection, back.input.selection);
        }
    }

    /// `StartupPage` round-trips through its string id form, including the
    /// `session:<id>` variant. Unknown ids fall back to `Home` so a stale
    /// `config.toml` can't brick launch.
    #[test]
    fn startup_page_id_roundtrip() {
        for page in [
            StartupPage::Home,
            StartupPage::Sftp,
            StartupPage::LocalTerminal,
            StartupPage::Session(42),
            StartupPage::Session(-1),
        ] {
            let id = page.to_id();
            assert_eq!(StartupPage::from_id(&id), page);
        }
        // Unknown / malformed ids fall back to Home.
        assert_eq!(StartupPage::from_id(""), StartupPage::Home);
        assert_eq!(StartupPage::from_id("bogus"), StartupPage::Home);
        assert_eq!(
            StartupPage::from_id("session:not-a-number"),
            StartupPage::Home
        );
    }

    /// `StartupPage` serializes into a single tagged string in `config.toml`,
    /// keeping the file readable and round-tripping through `to_id`/`from_id`.
    #[test]
    fn startup_page_serializes_as_string() {
        let toml = toml::to_string(&StartupPage::Session(7)).unwrap();
        assert!(toml.contains("\"session:7\""));
        let back: StartupPage = toml::from_str(&toml).unwrap();
        assert_eq!(back, StartupPage::Session(7));
    }
}
