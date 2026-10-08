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

    /// Global animation speed multiplier. Stored under `[appearance]` so it
    /// survives restarts. Affects every `duration_*()` token in
    /// `crabport-ui::motion` — higher tiers scale all transitions down by
    /// the corresponding factor (e.g. `Fast` = 0.75× → a 150 ms dialog
    /// fade completes in ~112 ms). `Standard` is the tuned baseline.
    #[serde(default)]
    pub animation_speed: AnimationSpeed,
}

fn default_panel_width() -> f32 {
    300.0
}

/// Which right-hand panel page a terminal shows by default.
///
/// Serialized as a single lowercase string (`panel_page = "ai"`) so the
/// config stays hand-editable. The default is the AI assistant page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PanelPage {
    Sftp,
    Tunnels,
    History,
    Snippets,
    #[default]
    Ai,
}

impl PanelPage {
    /// The string form written to `config.toml`.
    pub fn to_id(self) -> String {
        match self {
            PanelPage::Sftp => "sftp",
            PanelPage::Tunnels => "tunnels",
            PanelPage::History => "history",
            PanelPage::Snippets => "snippets",
            PanelPage::Ai => "ai",
        }
        .to_string()
    }

    /// Parse the string form. An unknown id falls back to the default page
    /// rather than failing the load: a hand-edited typo should cost the user
    /// this one setting, not the whole config.
    pub fn from_id(id: &str) -> Self {
        match id {
            "sftp" => PanelPage::Sftp,
            "tunnels" => PanelPage::Tunnels,
            "history" => PanelPage::History,
            "snippets" => PanelPage::Snippets,
            _ => PanelPage::default(),
        }
    }
}

impl serde::Serialize for PanelPage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_id())
    }
}

impl<'de> serde::Deserialize<'de> for PanelPage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_id(&String::deserialize(d)?))
    }
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

/// The launch target. Serialized as a single tagged string
/// (`"home"`, `"sftp"`, `"local_terminal"`, `"session:<id>"`)
/// so hand-editing `config.toml` stays readable — see the manual
/// `Serialize`/`Deserialize` impls below, which route through
/// [`StartupPage::to_id`] / [`StartupPage::from_id`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
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

impl Serialize for StartupPage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Single string form: `page = "session:7"` instead of the derive's
        // `page = { session = 7 }` table — readable and grep-able.
        s.serialize_str(&self.to_id())
    }
}

impl<'de> Deserialize<'de> for StartupPage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept both the documented string form and the legacy derive form
        // (`{ session = 7 }`) that earlier builds wrote into config.toml, so
        // upgrading never resets the user's startup page.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Id(String),
            Legacy(LegacyStartupPage),
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum LegacyStartupPage {
            Home,
            Sftp,
            LocalTerminal,
            Session(i64),
        }
        Ok(match Raw::deserialize(d)? {
            // Unknown / malformed ids fall back to Home inside from_id.
            Raw::Id(s) => StartupPage::from_id(&s),
            Raw::Legacy(LegacyStartupPage::Home) => StartupPage::Home,
            Raw::Legacy(LegacyStartupPage::Sftp) => StartupPage::Sftp,
            Raw::Legacy(LegacyStartupPage::LocalTerminal) => StartupPage::LocalTerminal,
            Raw::Legacy(LegacyStartupPage::Session(id)) => StartupPage::Session(id),
        })
    }
}

// ---------------------------------------------------------------------------
// Animation speed
// ---------------------------------------------------------------------------

/// Global animation speed tier. Serialized as a single snake_case word
/// (`slow` / `standard` / `fast` / `fastest`) so hand-editing `config.toml`
/// stays readable.
///
/// The tier maps to a multiplier applied to every `duration_*()` token in
/// `crabport-ui::motion` — higher tiers scale durations down by the listed
/// factor. `Standard` is the tuned baseline (multiplier 1.0×) that the
/// motion tokens were designed against.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnimationSpeed {
    /// 1.25× the baseline duration. For users who find the default motion
    /// too snappy and want transitions to ease in more noticeably.
    Slow,
    /// 1.0× — the tuned baseline. Every `duration_*()` token reads as its
    /// nominal `DURATION_*` constant value.
    #[default]
    Standard,
    /// 0.75× the baseline. Snappier without feeling instant; good for
    /// power users who want to cut motion overhead.
    Fast,
    /// 0.5× the baseline. Transitions are perceptible but very quick —
    /// the lower bound before motion starts feeling jumpy.
    Fastest,
}

impl AnimationSpeed {
    /// Multiplier applied to baseline durations. `Standard` returns 1.0.
    pub fn multiplier(self) -> f32 {
        match self {
            AnimationSpeed::Slow => 1.25,
            AnimationSpeed::Standard => 1.0,
            AnimationSpeed::Fast => 0.75,
            AnimationSpeed::Fastest => 0.5,
        }
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
            animation_speed: AnimationSpeed::default(),
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

    /// Whether the right-hand panel auto-expands when a terminal tab
    /// finishes connecting. Defaults to `true` so a fresh install mirrors
    /// the behavior of every prior release (panel slides in once the SSH /
    /// Telnet session reaches `Connected`, or immediately for local PTY).
    /// The user can turn it off in Settings if they prefer the terminal to
    /// occupy the full width and only open the panel on demand via the
    /// toolbar toggle button.
    ///
    /// This is a *default* — the per-tab toggle (`CrabportApp::panel_open`)
    /// still wins once the user has clicked the button. The setting just
    /// controls what `panel_open` defaults to for a tab that hasn't been
    /// toggled yet.
    #[serde(default = "default_expand_panel_on_connect")]
    pub expand_panel_on_connect: bool,

    /// Which right-hand panel page a terminal shows by default — the page a
    /// tab's panel opens on until the user picks a page for that tab (which
    /// then wins, exactly like [`Self::expand_panel_on_connect`] is a default
    /// for the panel's visibility).
    ///
    /// Defaults to the AI assistant page. A page that isn't available on the
    /// tab's backend (e.g. Tunnels on a Telnet session, or AI while it is
    /// disabled) falls back to the first page that is.
    #[serde(default)]
    pub panel_page: PanelPage,

    /// Per-slot visibility for the bottom toolbar, stored under
    /// `[appearance.terminal.toolbar]`. Each field defaults to `true` so a
    /// fresh install shows every available chip; the user toggles them
    /// via the gear context menu in the toolbar itself. Fields that don't
    /// exist in the struct (e.g. ones added in a later version) just get
    /// the `#[serde(default)]` value, so config round-trips cleanly across
    /// versions.
    #[serde(default)]
    pub toolbar: ToolbarVisibilityConfig,

    /// SSH / Telnet keepalive interval, in seconds.
    ///
    /// When greater than zero, the backend periodically sends a no-op probe
    /// (SSH `channel.env` with an empty value; Telnet `IAC NOP`) on this
    /// cadence. A failed probe is treated as a dead connection and surfaces
    /// `BackendEvent::Closed`, which lets auto-reconnect kick in.
    ///
    /// `0` disables keepalive entirely (the backend never probes). Defaults
    /// to 60 seconds — a conservative cadence that keeps NAT/firewall idle
    /// timeouts at bay without spamming the server.
    #[serde(default = "default_keepalive_interval_secs")]
    pub keepalive_interval_secs: u32,

    /// Whether the UI should automatically reconnect after an unexpected
    /// disconnect (i.e. a `BackendEvent::Closed` that wasn't triggered by the
    /// user closing the tab).
    ///
    /// Off by default so a server that kicks the session (bad credentials,
    /// `MaxSessions`, host-key mismatch, etc.) doesn't loop reconnects and
    /// spam the connection history. The user can opt in from Settings if
    /// they want resilience against transient network drops.
    ///
    /// The reconnect uses exponential backoff (1 → 2 → 4 → … → 30 s cap).
    #[serde(default)]
    pub auto_reconnect: bool,
}

fn default_terminal_font_size() -> f32 {
    13.0
}

/// Default for `TerminalConfig::expand_panel_on_connect` — `true` so a
/// fresh install mirrors the pre-setting behavior (panel slides in on
/// connect).
fn default_expand_panel_on_connect() -> bool {
    true
}

/// Default keepalive interval — 60 s. Conservative; keeps NAT/firewall
/// idle timeouts at bay without spamming the server.
fn default_keepalive_interval_secs() -> u32 {
    60
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_terminal_font_size(),
            expand_panel_on_connect: default_expand_panel_on_connect(),
            panel_page: PanelPage::default(),
            toolbar: ToolbarVisibilityConfig::default(),
            keepalive_interval_secs: default_keepalive_interval_secs(),
            auto_reconnect: false,
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

    /// Effective keepalive interval as a `Duration`. Returns `None` when
    /// keepalive is disabled (`0` or an absurdly large value clamped to a
    /// sane upper bound of 1 hour).
    pub fn effective_keepalive(&self) -> Option<std::time::Duration> {
        let secs = self.keepalive_interval_secs.min(3600);
        if secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(u64::from(secs)))
        }
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
                fg: "#e8ebf2", bg: "#1b1e26", cursor: "#ccd2e8",
                black: "#3a4150", red: "#e5726f", green: "#7cc38a", yellow: "#e4c46a",
                blue: "#8ba2e8", magenta: "#d597e6", cyan: "#5bc8d8", white: "#cdd2de",
                bright_black: "#7b8294", bright_red: "#f08e8b", bright_green: "#98d6a4",
                bright_yellow: "#efd98c", bright_blue: "#a6b8f0", bright_magenta: "#e2a9ef",
                bright_cyan: "#7ad6e4", bright_white: "#e8ebf2"),
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
                fg: "#d6dcf5", bg: "#242436", cursor: "#f5e0dc",
                black: "#414559", red: "#e57384", green: "#a8d2a6", yellow: "#e7d488",
                blue: "#8fb2f7", magenta: "#e3b3d9", cyan: "#a3ded3", white: "#c2cadf",
                bright_black: "#62667a", bright_red: "#ef9aae", bright_green: "#bce0b8",
                bright_yellow: "#f1e0a6", bright_blue: "#a6c2fa", bright_magenta: "#efc8e7",
                bright_cyan: "#b6e7de", bright_white: "#c3c8e0"),
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
                fg: "#b4bce0", bg: "#20223a", cursor: "#c0caf5",
                black: "#3d3f58", red: "#e98097", green: "#9fd08a", yellow: "#e6cf8f",
                blue: "#8fb0f8", magenta: "#c2a6f8", cyan: "#88d3ff", white: "#a6b0d6",
                bright_black: "#4e5275", bright_red: "#f0a4b3", bright_green: "#b4dcab",
                bright_yellow: "#f0dcae", bright_blue: "#9cb8f9", bright_magenta: "#cfb8fa",
                bright_cyan: "#9bd9ff", bright_white: "#c0caf5"),
            selection: tc!(ThemeSelection; bg: "#33467c"),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self::modern_dark()
    }
}

// ---------------------------------------------------------------------------
// AI assistant config
// ---------------------------------------------------------------------------

/// One configured AI provider endpoint (`[[ai.providers]]` in
/// `config.toml`). Non-secret only — the API key lives AES-256-GCM
/// encrypted in the store's `ai_secrets` table, keyed by [`Self::id`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiProviderConfig {
    /// Stable id generated when the entry is added (`p<unix_millis`),
    /// never reused; built-in entries keep their fixed id (see
    /// [`BUILTIN_PROVIDER_IDS`]). FK for the encrypted key in `ai_secrets`.
    pub id: String,
    /// Protocol backend, serialized as `type`: `"openai"` means
    /// OpenAI-compatible chat completions (`{base}/chat/completions`,
    /// `{base}/models`). `"anthropic"` is planned, not implemented yet.
    #[serde(rename = "type")]
    pub provider_type: String,
    /// Display name shown in the provider list and switchers, e.g.
    /// `"DeepSeek"` or `"My Gateway"`.
    pub name: String,
    /// API base URL, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// When set, this entry borrows the API key stored for another entry
    /// instead of owning one — see [`Self::effective_key_id`].
    ///
    /// Gateways that put several endpoints behind a single credential share
    /// one secret this way: OpenCode's Zen and Go endpoints are one account
    /// with two base URLs, so the settings pane shows them as a single
    /// "OpenCode" entry whose key is the account's, while each endpoint
    /// keeps its own base URL, model list and `active` pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

/// Stable id of the built-in DeepSeek entry. It is seeded into a fresh
/// `config.toml` and is the default `active` pointer. Built-in entries keep
/// their id forever (ids are FKs for the encrypted key), so it's a constant
/// rather than a generated `p<millis>`.
pub const DEFAULT_PROVIDER_ID: &str = "deepseek";

/// Stable id of the built-in OpenCode Zen entry (OpenCode's curated model
/// gateway, <https://opencode.ai/docs/zen>).
pub const OPENCODE_ZEN_PROVIDER_ID: &str = "opencode-zen";

/// Stable id of the built-in OpenCode Go entry (OpenCode's subscription
/// tier for open coding models, <https://opencode.ai/docs/go>).
pub const OPENCODE_GO_PROVIDER_ID: &str = "opencode-go";

/// Ids of the built-in provider entries, in display order. The settings
/// pane renders these without the editable type / name / endpoint fields
/// (the product fixes those) and marks them unremovable.
pub const BUILTIN_PROVIDER_IDS: &[&str] = &[
    DEFAULT_PROVIDER_ID,
    OPENCODE_ZEN_PROVIDER_ID,
    OPENCODE_GO_PROVIDER_ID,
];

/// Whether `id` names a built-in provider entry (see
/// [`BUILTIN_PROVIDER_IDS`]).
pub fn is_builtin_provider(id: &str) -> bool {
    BUILTIN_PROVIDER_IDS.contains(&id)
}

impl AiProviderConfig {
    /// Id under which this entry's API key is stored: the id of the entry it
    /// borrows from when [`Self::key_id`] is set, its own id otherwise. The
    /// store's `ai_api_key` / `set_ai_api_key` calls always go through this.
    pub fn effective_key_id(&self) -> &str {
        self.key_id.as_deref().unwrap_or(self.id.as_str())
    }

    /// The built-in entries seeded into a fresh config — and re-seeded by
    /// [`normalize`] into configs that lost them (older builds persisted an
    /// explicit `providers = []`, which skips serde defaults). Return order
    /// is the canonical display order.
    fn builtin_providers() -> Vec<Self> {
        vec![
            Self {
                id: DEFAULT_PROVIDER_ID.into(),
                provider_type: "openai".into(),
                name: "DeepSeek".into(),
                base_url: "https://api.deepseek.com/v1".into(),
                key_id: None,
            },
            // OpenCode's two gateways. Both speak OpenAI-compatible chat
            // completions at `{base}/chat/completions` and list their
            // models at `{base}/models` — one entry per endpoint, because
            // each serves its own model list and is billed separately, but
            // they share the account's single API key, so the settings pane
            // folds them into one "OpenCode" section (Zen owns the key, Go
            // borrows it) and the model picker labels their models "Zen:"
            // / "Go:".
            Self {
                id: OPENCODE_ZEN_PROVIDER_ID.into(),
                provider_type: "openai".into(),
                name: "OpenCode".into(),
                base_url: "https://opencode.ai/zen/v1".into(),
                key_id: None,
            },
            Self {
                id: OPENCODE_GO_PROVIDER_ID.into(),
                provider_type: "openai".into(),
                name: "OpenCode".into(),
                base_url: "https://opencode.ai/zen/go/v1".into(),
                key_id: Some(OPENCODE_ZEN_PROVIDER_ID.into()),
            },
        ]
    }
}

/// AI assistant preferences. Stored under `[ai]` in `config.toml`.
///
/// Providers are a list of endpoint entries with an `active` pointer, so
/// several endpoints can coexist (each with its own encrypted key) and the
/// AI panel can switch between them by only flipping `active`.
///
/// The model is deliberately NOT part of a provider entry: model lists are
/// fetched from the endpoint's `/models` API (see
/// [`crabport_ai::OpenAiProvider::list_models`]) or hand-configured in a
/// toml/json file; [`Self::model`] persists the model selected for the
/// active entry and is validated/reset by the UI when the provider changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Master switch for the AI assistant surface. On by default — the
    /// settings pane phrases the toggle as “禁用 AI” (disable), not
    /// “enable”.
    pub enabled: bool,
    /// Id of the provider entry in use. A stale/empty pointer (entry was
    /// deleted) falls back to the first entry at resolve time.
    pub active: String,
    /// Model selected for the active provider (free text, e.g.
    /// `deepseek-chat`). Empty = not configured yet.
    pub model: String,
    /// Configured provider endpoints, display order = list order.
    pub providers: Vec<AiProviderConfig>,
    /// What the agent may do with each tool without asking. Stored under
    /// `[ai.agent]`; skipped while it holds no overrides, so a config that
    /// never touched the agent page doesn't grow an empty table.
    #[serde(skip_serializing_if = "AiAgentConfig::is_empty")]
    pub agent: AiAgentConfig,
}

impl Default for AiConfig {
    /// AI is enabled out of the box with the built-in endpoints pre-seeded
    /// (see [`BUILTIN_PROVIDER_IDS`]), so the first-run flow is just “paste
    /// an API key”. “Add Provider” in the settings pane is for *additional*
    /// endpoints.
    fn default() -> Self {
        Self {
            enabled: true,
            active: DEFAULT_PROVIDER_ID.into(),
            model: String::new(),
            providers: AiProviderConfig::builtin_providers(),
            agent: AiAgentConfig::default(),
        }
    }
}

impl AiConfig {
    /// The provider entry the `active` pointer selects; a stale or empty
    /// pointer falls back to the first entry. `None` when no entries exist.
    pub fn active_provider(&self) -> Option<&AiProviderConfig> {
        self.providers
            .iter()
            .find(|p| p.id == self.active)
            .or_else(|| self.providers.first())
    }
}

/// What the agent is allowed to do with one tool without asking the user
/// first. Serialized lowercase (`"confirm"`, `"allow"`, `"deny"`) so
/// `config.toml` stays hand-editable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPermission {
    /// Ask for approval on every call. The default: an unconfigured tool
    /// always asks.
    #[default]
    Confirm,
    /// Run the call without asking. Its card still appears, showing the
    /// command and what came back.
    Allow,
    /// Refuse the call; the model is told the user's policy denied it.
    Deny,
}

/// Per-tool permissions for the AI agent. Stored under `[ai.agent]`.
///
/// Only tools the user has changed away from [`ToolPermission::Confirm`]
/// are stored — a missing entry means “ask”, which keeps the file (and the
/// meaning of a fresh install) minimal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiAgentConfig {
    /// Tool name → permission, e.g. `terminal_exec = "allow"`. The names are
    /// the agent's wire names; unknown ones are ignored by the agent.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolPermission>,
}

impl AiAgentConfig {
    /// The permission configured for `tool`. Unlisted tools — including
    /// names no tool ever uses — resolve to [`ToolPermission::Confirm`].
    pub fn permission(&self, tool: &str) -> ToolPermission {
        self.tools.get(tool).copied().unwrap_or_default()
    }

    /// Set one tool's permission. `Confirm` is the default, so it is stored
    /// as “no entry” rather than an explicit value.
    pub fn set_permission(&mut self, tool: &str, permission: ToolPermission) {
        if permission == ToolPermission::default() {
            self.tools.remove(tool);
        } else {
            self.tools.insert(tool.to_string(), permission);
        }
    }

    /// Drop every override, so every tool asks again.
    pub fn reset(&mut self) {
        self.tools.clear();
    }

    /// Whether any tool has an override. Used to keep the `[ai.agent]`
    /// table out of a config that never changed a permission.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
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

    /// AI assistant settings, stored under `[ai]`.
    #[serde(default)]
    pub ai: AiConfig,
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
    let mut cfg: CrabPortConfig = toml::from_str(&text)?;
    normalize(&mut cfg);
    Ok(cfg)
}

/// Repair loaded configs so built-in pieces survive older or hand-edited
/// files.
///
/// The built-in entries ([`BUILTIN_PROVIDER_IDS`]) are part of the product
/// surface (“默认就有”) and product-owned: their type, name, endpoint and
/// key-sharing (`key_id`) are not user settings, and a config that predates
/// one of those fields must not keep the stale value. So a missing entry is
/// inserted at its canonical index (keeping the built-ins grouped at the
/// front in [`BUILTIN_PROVIDER_IDS`] order) and an existing one is rewritten
/// from the current definition — which is how a config written before
/// OpenCode Go started sharing Zen's key picks that up. Re-seeding is
/// idempotent, and user entries plus their `active` choice are untouched.
fn normalize(cfg: &mut CrabPortConfig) {
    for (ix, builtin) in AiProviderConfig::builtin_providers()
        .into_iter()
        .enumerate()
    {
        match cfg.ai.providers.iter().position(|p| p.id == builtin.id) {
            Some(at) => cfg.ai.providers[at] = builtin,
            None => {
                let at = ix.min(cfg.ai.providers.len());
                cfg.ai.providers.insert(at, builtin);
            }
        }
    }
    if cfg.ai.active.is_empty() {
        cfg.ai.active = DEFAULT_PROVIDER_ID.into();
    }
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

    /// The default panel page is the AI assistant, and it round-trips
    /// through `config.toml` as a plain string; an unknown id (a hand-edit
    /// typo) falls back to the default instead of failing the load.
    #[test]
    fn panel_page_defaults_to_ai_and_roundtrips() {
        assert_eq!(TerminalConfig::default().panel_page, PanelPage::Ai);
        assert_eq!(PanelPage::default(), PanelPage::Ai);

        let toml = toml::to_string(&TerminalConfig {
            panel_page: PanelPage::Tunnels,
            ..TerminalConfig::default()
        })
        .unwrap();
        assert!(toml.contains("panel_page = \"tunnels\""), "{toml}");
        let back: TerminalConfig = toml::from_str(&toml).unwrap();
        assert_eq!(back.panel_page, PanelPage::Tunnels);

        // Missing field → the default; unknown value → the default too.
        let partial: TerminalConfig = toml::from_str("font_size = 14.0").unwrap();
        assert_eq!(partial.panel_page, PanelPage::Ai);
        let typo: TerminalConfig = toml::from_str("panel_page = \"typo\"").unwrap();
        assert_eq!(typo.panel_page, PanelPage::Ai);

        for page in [
            PanelPage::Sftp,
            PanelPage::Tunnels,
            PanelPage::History,
            PanelPage::Snippets,
            PanelPage::Ai,
        ] {
            assert_eq!(PanelPage::from_id(&page.to_id()), page);
        }
    }

    /// `StartupPage` serializes into a single tagged string in `config.toml`,
    /// keeping the file readable and round-tripping through `to_id`/`from_id`.
    #[test]
    fn startup_page_serializes_as_string() {
        let toml = toml::to_string(&StartupConfig {
            page: StartupPage::Session(7),
        })
        .unwrap();
        assert!(toml.contains("\"session:7\""));
        let back: StartupConfig = toml::from_str(&toml).unwrap();
        assert_eq!(back.page, StartupPage::Session(7));
    }

    /// Deserialization accepts both the string form and the legacy derive
    /// form (`page = { session = 7 }`) written by earlier builds, and falls
    /// back to `Home` on unknown ids instead of failing the whole config.
    #[test]
    fn startup_page_deserializes_legacy_and_fallback_forms() {
        let back: StartupConfig = toml::from_str("page = { session = 7 }").unwrap();
        assert_eq!(back.page, StartupPage::Session(7));

        let back: StartupConfig = toml::from_str("page = \"local_terminal\"").unwrap();
        assert_eq!(back.page, StartupPage::LocalTerminal);

        let back: StartupConfig = toml::from_str("page = \"whatever\"").unwrap();
        assert_eq!(back.page, StartupPage::Home);
    }

    /// The `[ai]` section round-trips through TOML (`type` is the serde
    /// name for the protocol field), and a config written before the
    /// section existed parses with the seeded default instead of erroring.
    #[test]
    fn ai_section_roundtrip_and_missing_defaults() {
        let cfg = AiConfig {
            enabled: false,
            active: "p1".into(),
            model: "deepseek-chat".into(),
            providers: vec![AiProviderConfig {
                id: "p1".into(),
                provider_type: "openai".into(),
                name: "My Gateway".into(),
                base_url: "https://api.deepseek.com/v1".into(),
                key_id: None,
            }],
            agent: AiAgentConfig::default(),
        };
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("type = \"openai\""));
        // No permission overrides → no `[ai.agent]` table is written at all.
        assert!(!text.contains("[agent]"));
        let back: AiConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);

        let empty: AiConfig = toml::from_str("").unwrap();
        assert_eq!(empty, AiConfig::default());
    }

    /// Tool permissions: unset tools ask, overrides round-trip through
    /// `[ai.agent.tools]`, and setting a tool back to `confirm` clears its
    /// entry instead of storing the default.
    #[test]
    fn agent_tool_permissions_roundtrip_and_default_to_confirm() {
        let mut agent = AiAgentConfig::default();
        assert_eq!(agent.permission("terminal_exec"), ToolPermission::Confirm);
        assert_eq!(
            agent.permission("never-heard-of-it"),
            ToolPermission::Confirm
        );

        agent.set_permission("terminal_exec", ToolPermission::Allow);
        agent.set_permission("sftp_download", ToolPermission::Deny);
        let text = toml::to_string(&AiConfig {
            agent: agent.clone(),
            ..AiConfig::default()
        })
        .unwrap();
        assert!(text.contains("terminal_exec = \"allow\""), "{text}");
        assert!(text.contains("sftp_download = \"deny\""), "{text}");

        let back: AiConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.agent, agent);
        assert_eq!(
            back.agent.permission("terminal_exec"),
            ToolPermission::Allow
        );
        assert_eq!(
            back.agent.permission("terminal_run"),
            ToolPermission::Confirm
        );

        // Back to the default → the key goes away again.
        agent.set_permission("terminal_exec", ToolPermission::Confirm);
        assert!(!agent.tools.contains_key("terminal_exec"));
        agent.reset();
        assert!(agent.tools.is_empty());
    }

    /// A config written before `[ai.agent]` existed parses with every tool
    /// asking for confirmation, and an unknown permission value in the file
    /// fails the parse rather than silently granting access.
    #[test]
    fn agent_section_missing_defaults_and_rejects_unknown_permissions() {
        let ai: AiConfig = toml::from_str("[agent]\n").unwrap();
        assert_eq!(
            ai.agent.permission("terminal_exec"),
            ToolPermission::Confirm
        );

        let bad = toml::from_str::<AiConfig>("[agent.tools]\nterminal_exec = \"whatever\"\n");
        assert!(bad.is_err());
    }

    /// A fresh default enables AI and seeds the built-in endpoints (DeepSeek
    /// plus the OpenCode gateways), so the first-run flow is just “paste an
    /// API key”.
    #[test]
    fn ai_defaults_enable_and_seed_builtin_providers() {
        let ai = AiConfig::default();
        assert!(ai.enabled);
        assert_eq!(ai.active, DEFAULT_PROVIDER_ID);
        let ids: Vec<&str> = ai.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, BUILTIN_PROVIDER_IDS);

        let entry = ai.active_provider().expect("seeded provider");
        assert_eq!(entry.id, DEFAULT_PROVIDER_ID);
        assert_eq!(entry.provider_type, "openai");
        assert_eq!(entry.base_url, "https://api.deepseek.com/v1");

        // Both OpenCode gateways are OpenAI-compatible: one base URL each,
        // Zen and Go sharing the `/zen` prefix.
        let zen = ai
            .providers
            .iter()
            .find(|p| p.id == OPENCODE_ZEN_PROVIDER_ID)
            .expect("seeded zen provider");
        assert_eq!(zen.provider_type, "openai");
        assert_eq!(zen.base_url, "https://opencode.ai/zen/v1");
        let go = ai
            .providers
            .iter()
            .find(|p| p.id == OPENCODE_GO_PROVIDER_ID)
            .expect("seeded go provider");
        assert_eq!(go.provider_type, "openai");
        assert_eq!(go.base_url, "https://opencode.ai/zen/go/v1");

        // Zen and Go are one OpenCode account behind two endpoints, so both
        // resolve the same stored key (Zen owns it, Go borrows it) and share
        // one display name — the settings pane renders the pair as a single
        // "OpenCode" section.
        assert_eq!(zen.effective_key_id(), OPENCODE_ZEN_PROVIDER_ID);
        assert_eq!(go.effective_key_id(), OPENCODE_ZEN_PROVIDER_ID);
        assert_eq!(zen.name, "OpenCode");
        assert_eq!(go.name, "OpenCode");
        assert_eq!(entry.effective_key_id(), DEFAULT_PROVIDER_ID);
    }

    /// `effective_key_id` follows a `key_id` borrow, and a borrowed key is
    /// the only thing it changes — ids and endpoints stay per entry.
    #[test]
    fn effective_key_id_falls_back_to_own_id() {
        let own = AiProviderConfig {
            id: "p1".into(),
            provider_type: "openai".into(),
            name: "Solo".into(),
            base_url: "https://example.com/v1".into(),
            key_id: None,
        };
        assert_eq!(own.effective_key_id(), "p1");

        let borrowing = AiProviderConfig {
            key_id: Some("p1".into()),
            ..own.clone()
        };
        assert_eq!(borrowing.effective_key_id(), "p1");
        assert_eq!(borrowing.id, "p1");

        // `key_id` is optional in TOML: configs written before it existed
        // parse with `None`, and `None` never serializes.
        let text = toml::to_string(&own).unwrap();
        assert!(!text.contains("key_id"));
        let back: AiProviderConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, own);
    }

    /// A config persisted by an older build (explicit `providers = []`),
    /// written before one of the built-ins existed, or hand-trimmed still
    /// gets every built-in entry re-seeded at its canonical index, and an
    /// empty `active` pointer is repaired — while user entries and their
    /// `active` choice survive untouched.
    #[test]
    fn normalize_reseeds_builtin_providers_idempotently() {
        let mut cfg: CrabPortConfig =
            toml::from_str("[ai]\nenabled = true\nactive = \"\"\nproviders = []\n").unwrap();
        normalize(&mut cfg);
        let ids: Vec<&str> = cfg.ai.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, BUILTIN_PROVIDER_IDS);
        assert_eq!(cfg.ai.active, DEFAULT_PROVIDER_ID);

        // Idempotent: running again adds nothing.
        normalize(&mut cfg);
        assert_eq!(cfg.ai.providers.len(), BUILTIN_PROVIDER_IDS.len());

        // A dropped built-in comes back at its canonical index, ahead of
        // user entries, and the user's `active` pick is left alone.
        let mut cfg = CrabPortConfig::default();
        cfg.ai
            .providers
            .retain(|p| p.id != OPENCODE_ZEN_PROVIDER_ID);
        cfg.ai.providers.push(AiProviderConfig {
            id: "p1".into(),
            provider_type: "openai".into(),
            name: "My Gateway".into(),
            base_url: String::new(),
            key_id: None,
        });
        cfg.ai.active = "p1".into();
        normalize(&mut cfg);
        let ids: Vec<&str> = cfg.ai.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                DEFAULT_PROVIDER_ID,
                OPENCODE_ZEN_PROVIDER_ID,
                OPENCODE_GO_PROVIDER_ID,
                "p1",
            ]
        );
        assert_eq!(cfg.ai.active, "p1");
    }

    /// Built-in entries are product-owned: a config written before Go
    /// started borrowing Zen's key (so it carries a stale/empty `key_id`) is
    /// repaired, while user entries are never touched.
    #[test]
    fn normalize_repairs_builtin_fields_but_not_user_entries() {
        let mut cfg = CrabPortConfig::default();
        let go = cfg
            .ai
            .providers
            .iter_mut()
            .find(|p| p.id == OPENCODE_GO_PROVIDER_ID)
            .expect("seeded go provider");
        go.key_id = None;
        go.base_url = "https://example.com/stale".into();

        normalize(&mut cfg);

        let go = cfg
            .ai
            .providers
            .iter()
            .find(|p| p.id == OPENCODE_GO_PROVIDER_ID)
            .expect("repaired go provider");
        assert_eq!(go.base_url, "https://opencode.ai/zen/go/v1");
        assert_eq!(go.effective_key_id(), OPENCODE_ZEN_PROVIDER_ID);

        // User entries keep whatever they were saved with.
        cfg.ai.providers.push(AiProviderConfig {
            id: "p1".into(),
            provider_type: "openai".into(),
            name: "My Gateway".into(),
            base_url: "https://example.com/v1".into(),
            key_id: Some("p2".into()),
        });
        normalize(&mut cfg);
        let user = cfg
            .ai
            .providers
            .iter()
            .find(|p| p.id == "p1")
            .expect("user entry kept");
        assert_eq!(user.base_url, "https://example.com/v1");
        assert_eq!(user.key_id.as_deref(), Some("p2"));
    }

    /// Built-in entries are recognised by id, and only by id.
    #[test]
    fn builtin_provider_ids_are_recognised() {
        assert!(is_builtin_provider(DEFAULT_PROVIDER_ID));
        assert!(is_builtin_provider(OPENCODE_ZEN_PROVIDER_ID));
        assert!(is_builtin_provider(OPENCODE_GO_PROVIDER_ID));
        assert!(!is_builtin_provider("p1723000000000"));
        assert!(!is_builtin_provider(""));
    }

    /// `active_provider` resolves the pointer, falls back to the first
    /// entry on a stale id, and is `None` when no entries exist.
    #[test]
    fn active_provider_falls_back_to_first_entry() {
        let mut cfg = AiConfig {
            providers: Vec::new(),
            ..AiConfig::default()
        };
        assert!(cfg.active_provider().is_none());

        let entry = |id: &str, name: &str| AiProviderConfig {
            id: id.into(),
            provider_type: "openai".into(),
            name: name.into(),
            base_url: String::new(),
            key_id: None,
        };
        cfg.providers = vec![entry("a", "A"), entry("b", "B")];

        cfg.active = "b".into();
        assert_eq!(cfg.active_provider().map(|p| p.id.as_str()), Some("b"));

        // Stale pointer (entry deleted) → first entry.
        cfg.active = "deleted".into();
        assert_eq!(cfg.active_provider().map(|p| p.id.as_str()), Some("a"));
    }
}
