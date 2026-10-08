//! Settings window.
//!
//! Renders a sidebar on the left (General / Appearance / Keybinds, plus the
//! AI providers and agent pages under an “AI” heading) and a scrollable
//! content pane on the right. Every control reads from and writes to the
//! process-wide [`crabport_core::config::CONFIG`] `LazyLock`, so changes are
//! persisted to `config.toml` immediately and visible to every other window
//! in the process.
//!
//! Sections are built declaratively via [`crate::windows::settings_section::Section`].

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use rust_i18n::t;

use crabport_agent::{TOOL_NAMES, ToolKind};
use crabport_core::config::{
    self, AiProviderConfig, AnimationSpeed, StartupPage, ToolPermission, is_builtin_provider,
};
use crabport_core::credential::HostEntry;

use crate::ai::PROVIDER_TYPES;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::input::StyledInput;
use crate::components::number_input::{StyledNumberInput, subscribe_number_filter};
use crate::components::segmented_control::{Segment, SegmentedControl};
use crate::components::settings_section::Section;
use crate::components::window_controls::{HAS_CLIENT_CONTROLS, WindowControls};
use crate::components::window_layout::{
    SidebarTabEntry, render_sidebar_window, render_tab_sidebar,
};
use crate::views::panel::ai::tool_title;

// ---------------------------------------------------------------------------
// Tab enum
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    General,
    Appearance,
    /// The right-hand panel's behavior (visibility on connect, default page).
    Panels,
    /// Session liveness: keepalive probes and auto-reconnect.
    Connection,
    Keybinds,
    Ai,
}

impl SettingsTab {
    const ALL: [SettingsTab; 6] = [
        SettingsTab::General,
        SettingsTab::Appearance,
        SettingsTab::Panels,
        SettingsTab::Connection,
        SettingsTab::Keybinds,
        SettingsTab::Ai,
    ];

    fn label(self) -> SharedString {
        match self {
            SettingsTab::General => t!("window.settings.tab.general").into(),
            SettingsTab::Appearance => t!("window.settings.tab.appearance").into(),
            SettingsTab::Panels => t!("window.settings.tab.panels").into(),
            SettingsTab::Connection => t!("window.settings.tab.connection").into(),
            SettingsTab::Keybinds => t!("window.settings.tab.keybinds").into(),
            SettingsTab::Ai => t!("window.settings.tab.ai").into(),
        }
    }

    fn sidebar_entries() -> Vec<SidebarTabEntry> {
        Self::ALL
            .iter()
            .enumerate()
            .map(|(i, tab)| SidebarTabEntry {
                id: ElementId::Name(format!("settings-tab-{i}").into()),
                label: tab.label(),
                icon: None,
            })
            .collect()
    }
}

/// One of the AI tab's second-level pages.
///
/// The AI tab is a landing page: the assistant's switch, then one row per
/// sub-page (Zed-settings style — a title, a description and a chevron that
/// opens the page, with a back arrow in its header). The provider entries and
/// the agent's tool permissions live on those pages instead of crowding the
/// landing page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AiSubPage {
    /// Provider endpoints and their API keys.
    Providers,
    /// Per-tool permissions for the agent.
    Agent,
}

impl AiSubPage {
    fn label(self) -> SharedString {
        match self {
            AiSubPage::Providers => t!("window.settings.ai.sub_providers").into(),
            AiSubPage::Agent => t!("window.settings.ai.sub_agent").into(),
        }
    }

    fn description(self) -> SharedString {
        match self {
            AiSubPage::Providers => t!("window.settings.ai.sub_providers_desc").into(),
            AiSubPage::Agent => t!("window.settings.ai.sub_agent_desc").into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default panel page
// ---------------------------------------------------------------------------

/// The panel pages the “default panel page” dropdown offers, in display
/// order (AI last, matching the panel's own tab order).
const PANEL_PAGES: [crabport_core::config::PanelPage; 5] = [
    crabport_core::config::PanelPage::Sftp,
    crabport_core::config::PanelPage::Tunnels,
    crabport_core::config::PanelPage::History,
    crabport_core::config::PanelPage::Snippets,
    crabport_core::config::PanelPage::Ai,
];

/// Label for one panel page.
fn panel_page_label(page: crabport_core::config::PanelPage) -> String {
    use crabport_core::config::PanelPage;
    let key = match page {
        PanelPage::Sftp => "window.settings.panels.page_sftp",
        PanelPage::Tunnels => "window.settings.panels.page_tunnels",
        PanelPage::History => "window.settings.panels.page_history",
        PanelPage::Snippets => "window.settings.panels.page_snippets",
        PanelPage::Ai => "window.settings.panels.page_ai",
    };
    t!(key).to_string()
}

// ---------------------------------------------------------------------------
// AI second-level pages
// ---------------------------------------------------------------------------

/// The AI tab's second-level pages, in the order their rows are listed.
const AI_SUB_PAGES: [AiSubPage; 2] = [AiSubPage::Providers, AiSubPage::Agent];

/// One second-level page's row on the AI landing page, in the shape Zed's
/// settings use: the title and its muted description on the left, a button
/// on the right that opens the page.
fn sub_page_row(sub_page: AiSubPage, handle: &Entity<SettingsWindow>) -> AnyElement {
    let h = handle.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .w_full()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(text_primary()))
                        .child(sub_page.label()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(sub_page.description()),
                ),
        )
        .child(
            // The chevron rides along in the content so it trails the label.
            Button::action(
                ElementId::Name(format!("settings-ai-open-{sub_page:?}").into()),
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(t!("window.settings.ai.open_page").to_string())
                    .child(
                        svg()
                            .path("icons/chevron-right.svg")
                            .size_4()
                            .flex_shrink_0()
                            .text_color(rgb(text_muted())),
                    ),
            )
            .flex_shrink_0()
            .on_click(move |_e, _w, cx| {
                h.update(cx, |view, cx| {
                    view.ai_sub_page = Some(sub_page);
                    cx.notify();
                });
            }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Agent tool permissions
// ---------------------------------------------------------------------------

/// The permission choices, in the order the agent page renders them.
/// `Confirm` first because it is the default: reading left-to-right, the
/// least autonomous choice comes first.
const PERMISSION_ORDER: [ToolPermission; 3] = [
    ToolPermission::Confirm,
    ToolPermission::Allow,
    ToolPermission::Deny,
];

/// i18n key for one permission's label.
fn permission_label_key(permission: ToolPermission) -> &'static str {
    match permission {
        ToolPermission::Confirm => "window.settings.ai.permission_confirm",
        ToolPermission::Allow => "window.settings.ai.permission_allow",
        ToolPermission::Deny => "window.settings.ai.permission_deny",
    }
}

/// Row label for one tool on the agent page. Mostly the card title; the two
/// command executors share one title (the cards tell them apart by their
/// fields), so they get labels that say which is which.
fn agent_tool_label(tool: &str) -> String {
    match tool {
        "terminal_exec" => t!("window.settings.ai.agent_tool_exec").to_string(),
        "terminal_run" => t!("window.settings.ai.agent_tool_run").to_string(),
        _ => tool_title(tool),
    }
}

// ---------------------------------------------------------------------------
// Root view
// ---------------------------------------------------------------------------

/// Root view for the Settings window.
pub struct SettingsWindow {
    tab: SettingsTab,
    /// Second-level page open on the AI tab, if any. `None` shows the tab's
    /// landing page.
    ai_sub_page: Option<AiSubPage>,
    // Dropdown open states (Dropdown is uncontrolled — caller manages it).
    locale_dropdown_open: bool,
    theme_dropdown_open: bool,
    font_family_dropdown_open: bool,
    startup_dropdown_open: bool,
    animation_speed_dropdown_open: bool,
    /// Open flag for the default-panel-page dropdown.
    panel_page_dropdown_open: bool,
    /// AI preset dropdown currently open — the entry id, or `None` when all
    /// are closed (only one dropdown open at a time).
    ai_open_dropdown: Option<String>,
    /// Per-entry input entities (base URL / model / masked API key),
    /// matched to `config.ai.providers` by entry id. Built on app start and
    /// extended/trimmed by add/remove; id-based lookups keep hand-edits of
    /// config.toml from desyncing the render.
    ai_entry_inputs: Vec<AiEntryInputs>,
    /// Search input backing the terminal font-family dropdown. Lets the
    /// user type to filter the (potentially long) list of installed fonts.
    font_search_input: Entity<InputState>,
    /// `InputState` backing the terminal font-size stepper. Pre-filled with
    /// the persisted size on open and re-clamped on every edit via
    /// [`subscribe_number_filter`].
    font_size_input: Entity<InputState>,
    /// Focus flag for the font-size input (drives the accent border).
    font_size_focused: bool,
    /// `InputState` backing the terminal keepalive-interval stepper. Pre-filled
    /// with the persisted value on open and re-clamped to `[0, 3600]` on every
    /// edit via [`subscribe_number_filter`] (`0` disables keepalive).
    keepalive_input: Entity<InputState>,
    /// Cached list of *all* system-installed font family names shown in the
    /// Terminal section's font dropdown. Built lazily on first render of the
    /// Appearance pane.
    mono_font_names: Vec<String>,
    /// The action_id currently capturing a keystroke, or `None` when idle.
    /// When `Some`, the next key press is recorded as the new binding for
    /// that action instead of being dispatched normally.
    recording_action: Option<String>,
    /// Focus handle used to capture keyboard input while recording a keybind.
    focus_handle: FocusHandle,
    /// Error message for the action currently being recorded, if any
    /// (e.g. conflict with another binding).
    keybind_error: Option<String>,
}

/// Input entities for one AI provider entry, matched to its
/// `config.ai.providers` row by stable id. Subscriptions persist edits
/// straight into config/store — see [`SettingsWindow::build_ai_entry_inputs`].
struct AiEntryInputs {
    entry_id: String,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
}

impl SettingsWindow {
    /// Open the Settings window (or no-op if one already exists — callers
    /// should normally go through [`crate::windows::focus_or_open`] for the
    /// singleton check).
    pub fn open(cx: &mut App) -> WindowHandle<gpui_component::Root> {
        let options = crate::windows::aux_window_options(
            t!("window.settings.title").to_string().into(),
            size(px(720.0), px(820.0)),
            size(px(560.0), px(440.0)),
            cx,
        );

        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let view = cx.new(|cx| SettingsWindow::new(window, cx));
                gpui_component::Root::new(view, window, cx)
            })
        })
        .expect("Failed to open Settings window")
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Pre-fill the font-size stepper with the persisted value so the
        // input shows the current size on first open rather than blank.
        let current_size = config::snapshot().appearance.terminal.effective_font_size() as i64;
        let font_size_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(current_size.to_string(), window, cx);
            state
        });
        // Enforce digits-only + clamp into [8, 32] on every edit, then
        // persist the cleaned value and repaint every window so each
        // terminal picks up the new size on its next render.
        subscribe_number_filter(&font_size_input, 8, 32, window, cx, |_this, value, cx| {
            let _ = config::update(|cfg| {
                cfg.appearance.terminal.font_size = value as f32;
            });
            cx.refresh_windows();
        })
        .detach();
        // Track focus so the stepper's accent border reflects keyboard
        // focus (mirrors how StyledInput expects a `focused` bool).
        cx.subscribe(
            &font_size_input,
            |this, _input, event: &InputEvent, cx| match event {
                InputEvent::Focus => {
                    this.font_size_focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.font_size_focused = false;
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();
        // Keepalive-interval stepper — same shape as font-size above but
        // clamped to `[0, 3600]` seconds. `0` disables keepalive entirely
        // (see `TerminalConfig::effective_keepalive`).
        let current_keepalive = config::snapshot()
            .appearance
            .terminal
            .keepalive_interval_secs as i64;
        let keepalive_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(current_keepalive.to_string(), window, cx);
            state
        });
        subscribe_number_filter(&keepalive_input, 0, 3600, window, cx, |_this, value, cx| {
            let _ = config::update(|cfg| {
                cfg.appearance.terminal.keepalive_interval_secs = value as u32;
            });
            cx.refresh_windows();
        })
        .detach();
        // AI provider entries — one input triple per configured endpoint,
        // built from the persisted config so the pane opens pre-filled.
        let ai_entry_inputs: Vec<AiEntryInputs> = config::snapshot()
            .ai
            .providers
            .iter()
            .map(|entry| Self::build_ai_entry_inputs(entry, window, cx))
            .collect();
        // Search box for the font-family dropdown — filters the list of
        // installed fonts by case-insensitive substring.
        let font_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("groups.search_placeholder").to_string())
        });
        Self {
            tab: SettingsTab::General,
            ai_sub_page: None,
            locale_dropdown_open: false,
            theme_dropdown_open: false,
            font_family_dropdown_open: false,
            startup_dropdown_open: false,
            animation_speed_dropdown_open: false,
            panel_page_dropdown_open: false,
            font_search_input,
            font_size_input,
            font_size_focused: false,
            keepalive_input,
            ai_open_dropdown: None,
            ai_entry_inputs,
            mono_font_names: Vec::new(),
            recording_action: None,
            focus_handle: cx.focus_handle(),
            keybind_error: None,
        }
    }

    // -------------------------------------------------------------------
    // Dropdown open-state plumbing
    // -------------------------------------------------------------------
    //
    // `Dropdown` is uncontrolled — the caller owns the open flag. Every
    // settings dropdown wires the same toggle / close-on-change dance, so
    // the two closures live here once, parameterized by a field accessor.

    /// Build an `on_toggle` handler that flips the given open flag.
    fn dropdown_toggle(
        handle: &Entity<Self>,
        slot: fn(&mut Self) -> &mut bool,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let handle = handle.clone();
        move |_w, cx| {
            handle.update(cx, |view, cx| {
                let open = slot(view);
                *open = !*open;
                cx.notify();
            });
        }
    }

    /// Clear the given open flag (the close-on-change tail).
    fn close_dropdown(handle: &Entity<Self>, slot: fn(&mut Self) -> &mut bool, cx: &mut App) {
        handle.update(cx, |view, cx| {
            *slot(view) = false;
            cx.notify();
        });
    }

    // -------------------------------------------------------------------
    // General pane (declarative sections)
    // -------------------------------------------------------------------

    fn render_general_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store_path = crabport_core::store::default_data_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string());
        let handle = cx.entity().clone();

        // ---- Startup page dropdown ----
        // Build from the saved-host list so the user can pin any session as
        // the launch target. Hosts come straight from the store (not the
        // in-memory `CrabportApp::hosts` list, which lives in the main
        // window and isn't reachable from here).
        let hosts: Vec<HostEntry> = AppState::store(cx).lock().hosts().unwrap_or_default();
        let current_page = config::snapshot().appearance.startup.page.clone();

        // Stable item list: Home / SFTP / Local Terminal, then every saved
        // host as `Session(<id>)`. Values use `StartupPage::to_id` so
        // `on_change` can resolve the selected index back to a page.
        let mut startup_items: Vec<(String, String)> = vec![
            (
                t!("window.settings.general.startup_page_home").to_string(),
                StartupPage::Home.to_id(),
            ),
            (
                t!("window.settings.general.startup_page_sftp").to_string(),
                StartupPage::Sftp.to_id(),
            ),
            (
                t!("window.settings.general.startup_page_local_terminal").to_string(),
                StartupPage::LocalTerminal.to_id(),
            ),
        ];
        for h in &hosts {
            startup_items.push((
                format!(
                    "{}{} ({})",
                    t!("window.settings.general.startup_page_session_prefix"),
                    h.name,
                    h.host
                ),
                StartupPage::Session(h.id).to_id(),
            ));
        }
        let startup_idx = startup_items
            .iter()
            .position(|(_, v)| *v == current_page.to_id())
            // Stale host id (host was deleted since the user last picked
            // it) → fall back to Home so the dropdown always shows a valid
            // entry. The launch-time fallback in `CrabportApp::wire` also
            // guards this, but normalizing here keeps the displayed value
            // honest.
            .or_else(|| startup_items.iter().position(|(_, v)| v == "home"))
            .unwrap_or(0);

        let startup_dropdown = {
            let h_for_change = handle.clone();
            let items_for_change = startup_items.clone();
            let mut dd = Dropdown::new("settings-startup-page")
                .is_open(self.startup_dropdown_open)
                .selected(startup_idx);
            for (label, value) in &startup_items {
                dd = dd.item_with_value(label.clone(), value.clone());
            }
            dd.on_toggle(Self::dropdown_toggle(&handle, |v| {
                &mut v.startup_dropdown_open
            }))
            .on_change(move |idx, _w, cx| {
                let page = items_for_change
                    .get(idx)
                    .map(|(_, v)| StartupPage::from_id(v))
                    .unwrap_or(StartupPage::Home);
                let _ = config::update(|cfg| {
                    cfg.appearance.startup.page = page;
                });
                Self::close_dropdown(&h_for_change, |v| &mut v.startup_dropdown_open, cx);
            })
        };

        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("settings-general-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                // --- Startup page section ---
                .child(
                    Section::new()
                        .header(t!("window.settings.general.section_startup"))
                        .desc(t!("window.settings.general.startup_page_desc"))
                        .field(
                            t!("window.settings.general.startup_page_label").to_string(),
                            div().w(px(280.0)).child(startup_dropdown),
                        ),
                )
                // --- Data directory section ---
                .child(
                    Section::new()
                        .header(t!("window.settings.general.section_data"))
                        .desc(t!("window.settings.general.open_data_dir_desc"))
                        .bare(
                            // A long path shrinks (rather than pushing the row
                            // wider) and ellipsizes.
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(store_path),
                        )
                        .bare(
                            Button::action(
                                "settings-open-data-dir",
                                t!("window.settings.general.open_data_dir").to_string(),
                            )
                            .on_click(move |_e, _w, cx| {
                                let _ = crabport_core::store::default_data_dir().map(|p| {
                                    let _ = open_path(&p, cx);
                                });
                            }),
                        ),
                )
                // --- Reset config section ---
                .child(
                    Section::new()
                        .header(t!("window.settings.general.reset_config"))
                        .desc(t!("window.settings.general.reset_config_desc"))
                        .bare({
                            let h = handle.clone();
                            Button::action(
                                "settings-reset-config",
                                t!("window.settings.general.reset_config").to_string(),
                            )
                            .on_click(move |_e, _w, cx| {
                                let _ = config::update(|cfg| {
                                    cfg.appearance = Default::default();
                                });
                                // Resetting appearance also resets the theme,
                                // so repaint every window with the default
                                // palette.
                                crate::refresh_theme_with(cx);
                                h.update(cx, |_, cx| {
                                    cx.notify();
                                });
                            })
                        }),
                ),
        )
    }

    // -------------------------------------------------------------------
    // AI tab: landing page + second-level pages
    // -------------------------------------------------------------------

    /// The AI tab's landing page: the assistant's master switch, then one row
    /// per second-level page (providers, agent). The pages themselves hold the
    /// settings — a row here only opens one.
    fn render_ai_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ai = config::snapshot().ai;
        let handle = cx.entity().clone();

        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("settings-ai-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                // --- Enabled section (phrased as a *disable* toggle: AI is
                // on by default) ---
                .child(
                    Section::new()
                        .header(t!("window.settings.ai.section_ai"))
                        .desc(t!("window.settings.ai.disable_desc"))
                        .field(
                            t!("window.settings.ai.disable").to_string(),
                            div().w(px(180.0)).flex().justify_end().child(
                                crate::components::switch::Switch::new("settings-ai-enabled")
                                    .checked(!ai.enabled)
                                    .on_change({
                                        let h = handle.clone();
                                        move |checked, _w, cx| {
                                            let _ =
                                                config::update(|cfg| cfg.ai.enabled = !*checked);
                                            h.update(cx, |_, cx| cx.notify());
                                        }
                                    }),
                            ),
                        ),
                )
                // --- One row per second-level page ---
                .child(AI_SUB_PAGES.iter().fold(
                    Section::new().header(t!("window.settings.ai.section_sections")),
                    |section, sub_page| section.bare(sub_page_row(*sub_page, &handle)),
                )),
        )
    }

    /// One of the AI tab's second-level pages: its own header (back arrow +
    /// breadcrumb) above the page body.
    fn render_ai_sub_page(&self, sub_page: AiSubPage, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let body: AnyElement = match sub_page {
            AiSubPage::Providers => self.render_ai_providers_page(cx).into_any_element(),
            AiSubPage::Agent => self.render_ai_agent_pane(cx).into_any_element(),
        };

        // Navigation stays on the AI tab (the sidebar keeps highlighting it);
        // this header is how the user gets back to the landing page.
        let back = Button::new("settings-ai-sub-page-back")
            .icon("icons/arrow-left.svg")
            .size_6()
            .centered(true)
            .flex_shrink_0()
            .on_click({
                let h = handle.clone();
                move |_e, _w, cx| {
                    h.update(cx, |view, cx| {
                        view.ai_sub_page = None;
                        cx.notify();
                    });
                }
            });
        let breadcrumb = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .min_w_0()
            .text_sm()
            .child(
                div()
                    .text_color(rgb(text_muted()))
                    .child(t!("window.settings.tab.ai").to_string()),
            )
            .child(div().text_color(rgb(text_muted())).child("/"))
            .child(
                div()
                    .truncate()
                    .text_color(rgb(text_primary()))
                    .child(sub_page.label()),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_6()
                    .pt_6()
                    .child(back)
                    .child(breadcrumb),
            )
            .child(body)
    }

    /// The providers page: one section per configured endpoint plus the
    /// “Add Provider” action.
    ///
    /// The root flexes to the height the sub-page header leaves, so the page
    /// scrolls inside that space instead of stretching the window content.
    fn render_ai_providers_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ai = config::snapshot().ai;
        let handle = cx.entity().clone();

        // Per-entry sections, in config list order. Entries whose inputs
        // are missing (e.g. config.toml hand-edited since open) are skipped,
        // and a built-in that borrows another entry's key is folded into its
        // owner's section — OpenCode's Zen and Go endpoints are one account
        // behind one key, so they read as a single "OpenCode" entry.
        let entry_sections: Vec<AnyElement> = ai
            .providers
            .iter()
            .filter(|entry| !(is_builtin_provider(&entry.id) && entry.key_id.is_some()))
            .filter_map(|entry| {
                let inputs = self
                    .ai_entry_inputs
                    .iter()
                    .find(|i| i.entry_id == entry.id)?;
                Some(self.render_ai_entry(entry, inputs, &handle, cx))
            })
            .collect();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .p_6()
            .gap_6()
            .child(
                div()
                    .id("settings-ai-providers-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        Section::new()
                            .header(t!("window.settings.ai.section_providers"))
                            .desc(t!("window.settings.ai.add_desc"))
                            .bare(
                                Button::action(
                                    "settings-ai-add",
                                    t!("window.settings.ai.add_provider").to_string(),
                                )
                                .on_click({
                                    let h = handle.clone();
                                    move |_e, w, cx| {
                                        h.update(cx, |view, cx| view.ai_add_provider(w, cx));
                                    }
                                }),
                            ),
                    )
                    // --- One section per configured provider entry ---
                    .children(entry_sections),
            )
    }

    /// The AI agent page: one permission control per tool.
    ///
    /// `Confirm` (the default) asks on every call; `Allow` runs a tool without
    /// asking — its card still shows the command and the result; `Deny`
    /// refuses it, and the model is told the user's policy declined. The
    /// tools come from the agent itself ([`TOOL_NAMES`]), grouped the way the
    /// panel colors their cards, and are labeled with the same titles the
    /// approval cards use.
    fn render_ai_agent_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let agent_cfg = config::snapshot().ai.agent;

        // Group the advertised tools the way the panel colors their cards:
        // terminal, then local files, network, tunnels and SFTP. Iterating
        // the agent's own name list (rather than a copy here) means a tool
        // added to the crate shows up in this page without any change.
        let mut groups: Vec<(&str, Vec<&'static str>)> = vec![
            ("window.settings.ai.agent_group_terminal", Vec::new()),
            ("window.settings.ai.agent_group_local", Vec::new()),
            ("window.settings.ai.agent_group_network", Vec::new()),
            ("window.settings.ai.agent_group_tunnel", Vec::new()),
            ("window.settings.ai.agent_group_sftp", Vec::new()),
        ];
        for &tool in TOOL_NAMES {
            let group = match ToolKind::of(tool) {
                ToolKind::TerminalRead | ToolKind::ExecCapture | ToolKind::Run => 0,
                ToolKind::LocalFs => 1,
                ToolKind::Fetch => 2,
                ToolKind::Tunnel => 3,
                ToolKind::Sftp => 4,
                // Unreachable for the advertised names; a hypothetical new
                // tool lands at the top rather than vanishing.
                ToolKind::Unknown => 0,
            };
            groups[group].1.push(tool);
        }

        let mut sections: Vec<AnyElement> = Vec::with_capacity(groups.len() + 1);
        sections.push(
            Section::new()
                .header(t!("window.settings.ai.section_agent"))
                .desc(t!("window.settings.ai.agent_desc"))
                .bare(
                    Button::action(
                        "settings-ai-agent-reset",
                        t!("window.settings.ai.agent_reset").to_string(),
                    )
                    .on_click({
                        let h = handle.clone();
                        move |_e, _w, cx| {
                            let _ = config::update(|cfg| cfg.ai.agent.reset());
                            h.update(cx, |_, cx| cx.notify());
                        }
                    }),
                )
                .into_any_element(),
        );

        for (header_key, tools) in groups {
            let mut section = Section::new().header(t!(header_key));
            for tool in tools {
                let current = agent_cfg.permission(tool);
                let active = PERMISSION_ORDER
                    .iter()
                    .position(|p| *p == current)
                    .unwrap_or(0);
                let mut control = SegmentedControl::new(ElementId::Name(
                    format!("settings-ai-permission-{tool}").into(),
                ))
                .active(active);
                for permission in PERMISSION_ORDER {
                    let h = handle.clone();
                    let tool = tool.to_string();
                    control = control.segment(
                        Segment::new(t!(permission_label_key(permission)).to_string()).on_select(
                            move |_w, cx| {
                                let _ = config::update(|cfg| {
                                    cfg.ai.agent.set_permission(&tool, permission);
                                });
                                h.update(cx, |_, cx| cx.notify());
                            },
                        ),
                    );
                }
                section = section.field(agent_tool_label(tool), div().w(px(240.0)).child(control));
            }
            sections.push(section.into_any_element());
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .p_6()
            .gap_6()
            .child(
                div()
                    .id("settings-ai-agent-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .children(sections),
            )
    }

    /// One provider entry. User-added entries show the full form (type /
    /// name / base URL / masked key + remove). Built-in entries (see
    /// [`BUILTIN_PROVIDER_IDS`](crabport_core::config::BUILTIN_PROVIDER_IDS))
    /// have no subtitle and expose only their API key — type, name and
    /// endpoint are fixed by the product. An entry that borrows another
    /// entry's key (`key_id`, e.g. OpenCode Go off Zen) shows a pointer to
    /// the owner instead of a key field of its own.
    fn render_ai_entry(
        &self,
        entry: &AiProviderConfig,
        inputs: &AiEntryInputs,
        handle: &Entity<Self>,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry_title: SharedString = {
            let type_label = crate::ai::provider_type_by_id(&entry.provider_type)
                .map(|tpe| t!(tpe.label_key).to_string())
                .unwrap_or_else(|| entry.provider_type.clone());
            if entry.name.trim().is_empty() {
                type_label
            } else {
                entry.name.clone()
            }
            .into()
        };
        // Either the entry's own masked key input, or — when it borrows
        // another entry's key — a muted pointer at the owner, whose field is
        // the one that writes the shared secret.
        let key_field = if let Some(owner_id) = entry.key_id.as_deref() {
            let owner = config::snapshot()
                .ai
                .providers
                .iter()
                .find(|p| p.id == owner_id)
                .and_then(|p| {
                    let label = if p.name.trim().is_empty() {
                        p.base_url.trim()
                    } else {
                        p.name.trim()
                    };
                    (!label.is_empty()).then(|| label.to_string())
                })
                .unwrap_or_else(|| owner_id.to_string());
            div()
                .w(px(280.0))
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("window.settings.ai.entry_shared_key", name = owner.as_str()).to_string())
                .into_any_element()
        } else {
            div()
                .w(px(280.0))
                .child(StyledInput::new(
                    format!("settings-ai-api-key-{}", entry.id),
                    inputs.api_key.clone(),
                ))
                .into_any_element()
        };

        // Built-in entries: type / name / endpoint are fixed by the
        // product, so the pane only exposes the API key — and the entry is
        // unremovable (config normalization re-seeds every built-in).
        if is_builtin_provider(&entry.id) {
            return Section::new()
                .header(entry_title)
                .field(
                    t!("window.settings.ai.entry_api_key").to_string(),
                    key_field,
                )
                .into_any_element();
        }

        let selected_idx = PROVIDER_TYPES
            .iter()
            .position(|tpe| tpe.id == entry.provider_type)
            .unwrap_or(0);
        let mut dd = Dropdown::new(ElementId::Name(
            format!("settings-ai-type-{}", entry.id).into(),
        ))
        .is_open(self.ai_open_dropdown.as_deref() == Some(entry.id.as_str()))
        .selected(selected_idx);
        for tpe in PROVIDER_TYPES {
            dd = dd.item_with_value(t!(tpe.label_key).to_string(), tpe.id.to_string());
        }
        let dd = dd
            .on_toggle({
                let h = handle.clone();
                let eid = entry.id.clone();
                move |_w, cx| {
                    h.update(cx, |view, cx| {
                        let open = view.ai_open_dropdown.as_deref() == Some(eid.as_str());
                        view.ai_open_dropdown = if open { None } else { Some(eid.clone()) };
                        cx.notify();
                    });
                }
            })
            .on_change({
                let h = handle.clone();
                let eid = entry.id.clone();
                move |idx, _w, cx| {
                    if let Some(tpe) = PROVIDER_TYPES.get(idx) {
                        let _ = config::update(|cfg| {
                            if let Some(e) = cfg.ai.providers.iter_mut().find(|e| e.id == eid) {
                                e.provider_type = tpe.id.into();
                            }
                        });
                    }
                    h.update(cx, |view, cx| {
                        view.ai_open_dropdown = None;
                        cx.notify();
                    });
                }
            });

        let eid = entry.id.clone();
        let h = handle.clone();
        Section::new()
            .header(entry_title)
            .field(
                t!("window.settings.ai.entry_type").to_string(),
                div().w(px(280.0)).child(dd),
            )
            .field(
                t!("window.settings.ai.entry_name").to_string(),
                div().w(px(280.0)).child(StyledInput::new(
                    format!("settings-ai-name-{}", entry.id),
                    inputs.name.clone(),
                )),
            )
            .field(
                t!("window.settings.ai.entry_base_url").to_string(),
                div().w(px(280.0)).child(StyledInput::new(
                    format!("settings-ai-base-url-{}", entry.id),
                    inputs.base_url.clone(),
                )),
            )
            .field(
                t!("window.settings.ai.entry_api_key").to_string(),
                key_field,
            )
            .bare(
                Button::action(
                    ElementId::Name(format!("settings-ai-remove-{}", entry.id).into()),
                    t!("window.settings.ai.remove_provider").to_string(),
                )
                .on_click(move |_e, _w, cx| {
                    h.update(cx, |view, cx| view.ai_remove_provider(&eid, cx));
                }),
            )
            .into_any_element()
    }

    /// Build the input trio for one entry and wire per-keystroke
    /// persistence: name + base URL into `config.toml` (`[ai]`), API key
    /// into the store's encrypted `ai_secrets` table. Subscriptions match
    /// the entry by id, so a hand-deleted entry turns the write into a
    /// no-op instead of resurrecting it.
    fn build_ai_entry_inputs(
        entry: &AiProviderConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AiEntryInputs {
        let name = cx.new(|cx| {
            let mut s = InputState::new(window, cx)
                .placeholder(t!("window.settings.ai.entry_name_placeholder").to_string());
            s.set_value(entry.name.clone(), window, cx);
            s
        });
        let base_url = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("https://api.openai.com/v1");
            s.set_value(entry.base_url.clone(), window, cx);
            s
        });
        let stored_key = AppState::store(cx)
            .lock()
            .ai_api_key(entry.effective_key_id())
            .ok()
            .flatten()
            .unwrap_or_default();
        let api_key = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("sk-…");
            state.set_masked(true, window, cx);
            state.set_value(stored_key.clone(), window, cx);
            state
        });

        let eid = entry.id.clone();
        cx.subscribe(&name, move |_this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                let _ = config::update(|cfg| {
                    if let Some(e) = cfg.ai.providers.iter_mut().find(|e| e.id == eid) {
                        e.name = text;
                    }
                });
            }
        })
        .detach();
        let eid = entry.id.clone();
        cx.subscribe(&base_url, move |_this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                let _ = config::update(|cfg| {
                    if let Some(e) = cfg.ai.providers.iter_mut().find(|e| e.id == eid) {
                        e.base_url = text;
                    }
                });
            }
        })
        .detach();
        let eid = entry.effective_key_id().to_string();
        cx.subscribe(&api_key, move |_this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                let _ = AppState::store(cx).lock().set_ai_api_key(&eid, &text);
            }
        })
        .detach();

        AiEntryInputs {
            entry_id: entry.id.clone(),
            name,
            base_url,
            api_key,
        }
    }

    /// Append a new OpenAI-compatible entry (empty fields) and build its
    /// inputs. Anthropic is not selectable yet — see `PROVIDER_TYPES`.
    fn ai_add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = format!(
            "p{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let entry = AiProviderConfig {
            id: id.clone(),
            provider_type: "openai".into(),
            name: String::new(),
            base_url: String::new(),
            key_id: None,
        };
        let inputs = Self::build_ai_entry_inputs(&entry, window, cx);
        self.ai_entry_inputs.push(inputs);
        self.ai_open_dropdown = None;
        let _ = config::update(|cfg| cfg.ai.providers.push(entry));
        cx.notify();
    }

    /// Drop the entry from config + its inputs, and delete its stored API
    /// key so it doesn't linger on disk. Built-in entries are not
    /// removable — [`normalize`](crabport_core::config) would re-seed them.
    fn ai_remove_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        if is_builtin_provider(id) {
            return;
        }
        self.ai_entry_inputs.retain(|i| i.entry_id != id);
        let _ = config::update(|cfg| cfg.ai.providers.retain(|p| p.id != id));
        if self.ai_open_dropdown.as_deref() == Some(id) {
            self.ai_open_dropdown = None;
        }
        let _ = AppState::store(cx).lock().delete_ai_api_key(id);
        cx.notify();
    }

    // -------------------------------------------------------------------
    // Appearance pane (declarative sections)
    // -------------------------------------------------------------------

    fn render_appearance_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let locale_idx = if config::snapshot().appearance.locale == "zh-CN" {
            1
        } else {
            0
        };

        // Theme dropdown is built from the merged catalog (built-in +
        // user-supplied `.toml` files under {data_dir}/crabport/themes/).
        // See `crate::theme`. The catalog is refreshed on startup and whenever
        // the user adds/removes a theme file — but we only snapshot it once
        // per render here (and clone the id list into the on_change closure).
        let themes = crate::theme::list();
        let current_name = config::snapshot().appearance.theme.name;
        let theme_idx = themes
            .iter()
            .position(|t| t.id == current_name.as_str())
            .unwrap_or(0);

        // Lazily build the font family list on first render.
        if self.mono_font_names.is_empty() {
            self.mono_font_names = collect_monospace_fonts(cx);
        }
        let mono_fonts = self.mono_font_names.clone();

        // --- Language dropdown ---
        let locale_dropdown = {
            let h_for_change = handle.clone();
            Dropdown::new("settings-locale")
                .item(t!("window.settings.appearance.language_en"))
                .item(t!("window.settings.appearance.language_zh_cn"))
                .selected(locale_idx)
                .is_open(self.locale_dropdown_open)
                .on_toggle(Self::dropdown_toggle(&handle, |v| {
                    &mut v.locale_dropdown_open
                }))
                .on_change(move |idx, _w, cx| {
                    let locale = if idx == 1 { "zh-CN" } else { "en" };
                    let _ = config::update(|cfg| {
                        cfg.appearance.locale = locale.to_string();
                    });
                    crate::set_locale(locale);
                    cx.refresh_windows();
                    Self::close_dropdown(&h_for_change, |v| &mut v.locale_dropdown_open, cx);
                })
        };

        // --- Theme dropdown ---
        let theme_dropdown = {
            let h_for_change = handle.clone();
            let mut dropdown = Dropdown::new("settings-theme");
            for t in &themes {
                dropdown = dropdown.item_with_value(t.dropdown_label(), t.id.clone());
            }
            dropdown
                .selected(theme_idx)
                .is_open(self.theme_dropdown_open)
                .on_toggle(Self::dropdown_toggle(&handle, |v| {
                    &mut v.theme_dropdown_open
                }))
                .on_change(move |idx, _w, cx| {
                    // `idx` is the position in the catalog snapshot captured
                    // above; resolve it back to an id via the same list, then
                    // apply via the catalog (handles built-in + custom).
                    let id = themes
                        .get(idx)
                        .map(|t| t.id.clone())
                        .unwrap_or_else(|| "modern-dark".to_string());
                    crate::color::apply_theme(&id);
                    crate::refresh_theme_with(cx);
                    Self::close_dropdown(&h_for_change, |v| &mut v.theme_dropdown_open, cx);
                })
        };

        // --- Font family dropdown ---
        let term_cfg = config::snapshot().appearance.terminal;
        let current_family = term_cfg.effective_font_family().to_string();
        let font_idx = mono_fonts
            .iter()
            .position(|f| *f == current_family)
            .unwrap_or(0);

        let font_family_dropdown = {
            let h_for_toggle = handle.clone();
            let h_for_change = handle.clone();
            let search_for_toggle = self.font_search_input.clone();
            let search_for_change = self.font_search_input.clone();
            let names = mono_fonts.clone();
            let mut dd = Dropdown::new("settings-term-font")
                .is_open(self.font_family_dropdown_open)
                .selected(font_idx)
                .searchable(self.font_search_input.clone())
                .on_toggle(move |_w, cx| {
                    let search = search_for_toggle.clone();
                    h_for_toggle.update(cx, |view, cx| {
                        // Clear the search query on close so the next open
                        // shows the full font list.
                        if view.font_family_dropdown_open {
                            search.update(cx, |s, cx| {
                                s.set_value("", _w, cx);
                            });
                        }
                        view.font_family_dropdown_open = !view.font_family_dropdown_open;
                        cx.notify();
                    });
                });
            for name in &mono_fonts {
                dd = dd.item(name.clone());
            }
            dd.on_change(move |idx, _w, cx| {
                if let Some(name) = names.get(idx) {
                    let _ = config::update(|cfg| {
                        cfg.appearance.terminal.font_family = name.clone();
                    });
                    cx.refresh_windows();
                }
                h_for_change.update(cx, |view, cx| {
                    view.font_family_dropdown_open = false;
                    search_for_change.update(cx, |s, cx| {
                        s.set_value("", _w, cx);
                    });
                    cx.notify();
                });
            })
        };

        // --- Font size stepper ---
        let font_size_stepper =
            StyledNumberInput::new("settings-term-font-size", self.font_size_input.clone())
                .focused(self.font_size_focused)
                .min(8)
                .max(32)
                .step(1);

        // --- Animation speed dropdown ---
        // Four tiers map to multipliers 1.25× / 1.0× / 0.75× / 0.5×. The
        // selected index is resolved back to an `AnimationSpeed` variant
        // via the parallel `animation_speed_items` list. On change we both
        // persist to config and push the live multiplier into `motion.rs`
        // so every open window picks up the new speed on its next frame —
        // no restart needed.
        let current_speed = config::snapshot().appearance.animation_speed;
        let animation_speed_items: [(AnimationSpeed, &str); 4] = [
            (
                AnimationSpeed::Slow,
                "window.settings.appearance.animation_speed_slow",
            ),
            (
                AnimationSpeed::Standard,
                "window.settings.appearance.animation_speed_standard",
            ),
            (
                AnimationSpeed::Fast,
                "window.settings.appearance.animation_speed_fast",
            ),
            (
                AnimationSpeed::Fastest,
                "window.settings.appearance.animation_speed_fastest",
            ),
        ];
        let speed_idx = animation_speed_items
            .iter()
            .position(|(s, _)| *s == current_speed)
            .unwrap_or(1);
        let animation_speed_dropdown = {
            let h_for_change = handle.clone();
            let items_for_change = animation_speed_items.clone();
            let mut dd = Dropdown::new("settings-animation-speed")
                .is_open(self.animation_speed_dropdown_open)
                .selected(speed_idx);
            for (_, key) in &animation_speed_items {
                dd = dd.item_with_value(t!(*key).to_string(), (*key).to_string());
            }
            dd.on_toggle(Self::dropdown_toggle(&handle, |v| {
                &mut v.animation_speed_dropdown_open
            }))
            .on_change(move |idx, _w, cx| {
                let speed = items_for_change
                    .get(idx)
                    .map(|(s, _)| *s)
                    .unwrap_or(AnimationSpeed::Standard);
                let _ = config::update(|cfg| {
                    cfg.appearance.animation_speed = speed;
                });
                // Push the new multiplier into the global motion cache so
                // every `duration_*()` call picks it up on the next render.
                //
                // We deliberately do NOT call `gpui_animation::reset_all_transitions()`
                // here: clearing the `states` map forces the next render's
                // `with_state_default` to rebuild every element's state from
                // `Default::default()`, and `animated_handle` then sees a diff
                // between the default and the live (e.g. hovered) style —
                // launching a transition from the default to the current
                // visual for EVERY element on screen, which looks far worse
                // than the alternative.
                //
                // Instead, leave the registry alone. Active animations keep
                // their old `duration` and finish within ≤320ms (the longest
                // baseline token). New transitions started after this point
                // pick up the new multiplier via the `duration_*()` calls.
                // The handoff is imperceptible because the only visible
                // effect of the speed change is on animations that START
                // after the switch — and those use the new value.
                //
                // `refresh_windows` schedules a repaint of every live window
                // so open views pick up the new multiplier on their next
                // render. (Debug builds show transient stutter on speed
                // change because `animation_tick`'s per-frame `refresh_windows`
                // + DashMap lock contention is amplified without compiler
                // optimizations; release builds don't exhibit this.)
                crate::motion::set_speed_multiplier(speed.multiplier());
                cx.refresh_windows();
                Self::close_dropdown(&h_for_change, |v| &mut v.animation_speed_dropdown_open, cx);
            })
        };

        // Build the pane from declarative sections.
        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("settings-appearance-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                // --- Language ---
                .child(
                    Section::new()
                        .header(t!("window.settings.appearance.section_language"))
                        .bare(div().w(px(240.0)).child(locale_dropdown)),
                )
                // --- Theme ---
                .child(
                    Section::new()
                        .header(t!("window.settings.appearance.section_theme"))
                        .desc(t!("window.settings.appearance.theme_desc"))
                        .bare(div().w(px(240.0)).child(theme_dropdown))
                        // Opens the Theme Editor window seeded from the theme
                        // that's currently applied.
                        .bare(
                            Button::action(
                                "settings-edit-theme",
                                t!("window.settings.appearance.edit_theme").to_string(),
                            )
                            .on_click(|_e, _w, cx| {
                                crate::windows::registry::focus_or_open(
                                    crate::windows::AuxWindowKind::ThemeEditor,
                                    cx,
                                );
                            }),
                        ),
                )
                // --- Terminal font ---
                .child(
                    Section::new()
                        .header(t!("window.settings.appearance.section_terminal"))
                        .desc(t!("window.settings.appearance.terminal_desc"))
                        .field(
                            t!("window.settings.appearance.terminal_font_family").to_string(),
                            div().w(px(240.0)).child(font_family_dropdown),
                        )
                        .field(
                            t!("window.settings.appearance.terminal_font_size").to_string(),
                            div().w(px(180.0)).child(font_size_stepper),
                        ),
                )
                // --- Animation speed ---
                .child(
                    Section::new()
                        .header(t!("window.settings.appearance.section_animation"))
                        .desc(t!("window.settings.appearance.animation_speed_desc"))
                        .field(
                            t!("window.settings.appearance.animation_speed_label").to_string(),
                            div().w(px(180.0)).child(animation_speed_dropdown),
                        ),
                ),
        )
    }

    // -------------------------------------------------------------------
    // Panels pane (right-hand panel behavior)
    // -------------------------------------------------------------------

    /// How the right-hand panel behaves for a terminal tab.
    ///
    /// Both controls are *defaults*: the panel's own tab strip and the
    /// toolbar's toggle record a per-tab choice, and that choice wins — these
    /// only decide what a tab shows before it has one.
    fn render_panels_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let term_cfg = config::snapshot().appearance.terminal;

        // --- Default panel page ---
        let panel_page_idx = PANEL_PAGES
            .iter()
            .position(|p| *p == term_cfg.panel_page)
            .unwrap_or(PANEL_PAGES.len() - 1);
        let panel_page_dropdown = {
            let h_for_change = handle.clone();
            let mut dd = Dropdown::new("settings-panel-page")
                .is_open(self.panel_page_dropdown_open)
                .selected(panel_page_idx);
            for page in PANEL_PAGES {
                dd = dd.item(panel_page_label(page));
            }
            dd.on_toggle(Self::dropdown_toggle(&handle, |v| {
                &mut v.panel_page_dropdown_open
            }))
            .on_change(move |idx, _w, cx| {
                let page = PANEL_PAGES.get(idx).copied().unwrap_or_default();
                let _ = config::update(|cfg| {
                    cfg.appearance.terminal.panel_page = page;
                });
                Self::close_dropdown(&h_for_change, |v| &mut v.panel_page_dropdown_open, cx);
            })
        };

        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("settings-panels-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                .child(
                    Section::new()
                        .header(t!("window.settings.panels.section"))
                        .desc(t!("window.settings.panels.desc"))
                        .field(
                            t!("window.settings.panels.expand_panel").to_string(),
                            div().w(px(180.0)).flex().justify_end().child(
                                crate::components::switch::Switch::new(
                                    "settings-panel-expand-on-connect",
                                )
                                .checked(term_cfg.expand_panel_on_connect)
                                .on_change({
                                    let h = handle.clone();
                                    move |checked, _w, cx| {
                                        let _ = config::update(|cfg| {
                                            cfg.appearance.terminal.expand_panel_on_connect =
                                                *checked;
                                        });
                                        // Repaint every window so open terminal
                                        // tabs pick up the new default on
                                        // their next render.
                                        let _ = h.update(cx, |_, cx| cx.notify());
                                    }
                                }),
                            ),
                        )
                        .field(
                            t!("window.settings.panels.page").to_string(),
                            div().w(px(180.0)).child(panel_page_dropdown),
                        ),
                ),
        )
    }

    // -------------------------------------------------------------------
    // Connection pane (liveness)
    // -------------------------------------------------------------------

    /// Keepalive probes and auto-reconnect: how a session survives idle
    /// periods and what happens when it drops anyway.
    fn render_connection_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let term_cfg = config::snapshot().appearance.terminal;

        // Keepalive interval stepper — `min 0` lets the user disable
        // keepalive by clearing the field down to 0 (matches
        // `effective_keepalive`).
        let keepalive_stepper =
            StyledNumberInput::new("settings-conn-keepalive", self.keepalive_input.clone())
                .min(0)
                .max(3600)
                .step(1);

        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("settings-connection-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                .child(
                    Section::new()
                        .header(t!("window.settings.connection.section"))
                        .desc(t!("window.settings.connection.desc"))
                        .field(
                            t!("window.settings.connection.keepalive").to_string(),
                            div().w(px(180.0)).child(keepalive_stepper),
                        )
                        .field(
                            t!("window.settings.connection.auto_reconnect").to_string(),
                            div().w(px(180.0)).flex().justify_end().child(
                                crate::components::switch::Switch::new("settings-auto-reconnect")
                                    .checked(term_cfg.auto_reconnect)
                                    .on_change({
                                        let h = handle.clone();
                                        move |checked, _w, cx| {
                                            let _ = config::update(|cfg| {
                                                cfg.appearance.terminal.auto_reconnect = *checked;
                                            });
                                            let _ = h.update(cx, |_, cx| cx.notify());
                                        }
                                    }),
                            ),
                        ),
                ),
        )
    }

    // -------------------------------------------------------------------
    // Keybinds pane
    // -------------------------------------------------------------------

    fn render_keybinds_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::kbd::Kbd;

        let handle = cx.entity().clone();
        let bindings = crate::keybinds::resolve_all();
        let recording = self.recording_action.clone();
        let error_msg = self.keybind_error.clone();

        let mut section = Section::new()
            .header(t!("window.settings.keybinds.section_shortcuts"))
            .desc(t!("window.settings.keybinds.shortcuts_desc"));

        for rb in &bindings {
            // Skip non-configurable entries (Quit, Hide, Tab, etc.) — they
            // are still registered but not shown in the settings UI.
            if !rb.entry.configurable {
                continue;
            }
            let action_id = rb.entry.action_id;
            let label = t!(rb.entry.label_key).to_string();
            let keystroke = rb.keystroke.clone();
            let is_recording = recording.as_deref() == Some(action_id);
            let has_error = error_msg.as_ref().is_some_and(|e| !e.is_empty()) && is_recording;

            // Build the Kbd display element.
            let kbd_el: AnyElement = if keystroke.is_empty() {
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(t!("window.settings.keybinds.unbound").to_string())
                    .into_any_element()
            } else if let Ok(stroke) = gpui::Keystroke::parse(&keystroke) {
                Kbd::new(stroke).into_any_element()
            } else {
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(keystroke.clone())
                    .into_any_element()
            };

            // Chip label: "Press keys…" while recording, otherwise the kbd.
            let chip_child: AnyElement = if is_recording {
                div()
                    .text_xs()
                    .text_color(rgb(text_primary()))
                    .child(t!("window.settings.keybinds.listening").to_string())
                    .into_any_element()
            } else {
                kbd_el
            };

            let row = div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .gap_4()
                // Label + optional error message
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().text_color(rgb(text_primary())).child(label))
                        .when_some(
                            if has_error { error_msg.clone() } else { None },
                            |el, msg| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(input_border_error()))
                                        .child(msg),
                                )
                            },
                        ),
                )
                // Clickable Kbd chip (left-click = rebind, right-click = clear):
                // the shared button shape, with the right-click carried by the
                // wrapper — the Button component models left clicks only.
                .child(
                    div()
                        .on_mouse_down(MouseButton::Right, {
                            let h = handle.clone();
                            let aid = action_id.to_string();
                            move |_e, _w, cx| {
                                cx.stop_propagation();
                                h.update(cx, |view, cx| {
                                    crate::keybinds::set_binding(&aid, "", cx);
                                    view.recording_action = None;
                                    view.keybind_error = None;
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            Button::new(SharedString::from(format!("keybind-chip-{action_id}")))
                                .child(chip_child)
                                .min_w(px(72.0))
                                .px_3()
                                // Recording is the button's selected state.
                                .selected(is_recording)
                                .cursor_pointer()
                                .on_click({
                                    let h = handle.clone();
                                    let aid = action_id.to_string();
                                    move |_e, w, cx| {
                                        h.update(cx, |view, cx| {
                                            view.keybind_error = None;
                                            if view.recording_action.as_deref() == Some(&aid) {
                                                view.recording_action = None;
                                            } else {
                                                view.recording_action = Some(aid.clone());
                                                view.focus_handle.focus(w);
                                            }
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                );

            section = section.bare(row);
        }

        let reset_all = Section::new()
            .header(t!("window.settings.keybinds.reset_all"))
            .desc(t!("window.settings.keybinds.reset_all_desc"))
            .bare(
                Button::action(
                    "settings-reset-all-keybinds",
                    t!("window.settings.keybinds.reset_all").to_string(),
                )
                .on_click({
                    let h = handle.clone();
                    move |_e, _w, cx| {
                        h.update(cx, |view, cx| {
                            crate::keybinds::reset_all_bindings(cx);
                            view.recording_action = None;
                            view.keybind_error = None;
                            cx.notify();
                        });
                    }
                }),
            );

        div().size_full().flex().flex_col().p_6().gap_6().child(
            div()
                .id("keybinds-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_6()
                .child(section)
                .child(reset_all),
        )
    }

    /// `on_key_down` handler attached to the view root while recording a
    /// keybind. Normalizes the keystroke, checks for conflicts, then either
    /// saves the binding or shows an error.
    fn on_key_recording(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Escape cancels.
        if event.keystroke.key.as_str() == "escape" {
            self.recording_action = None;
            self.keybind_error = None;
            cx.notify();
            return;
        }

        let Some(action_id) = self.recording_action.clone() else {
            return;
        };

        let Some(ks) = crate::keybinds::normalize_recorded_keystroke(event) else {
            return;
        };

        // Check for conflicts with other actions.
        if let Some((_, conflicting_label_key)) = crate::keybinds::find_conflict(&action_id, &ks) {
            self.keybind_error = Some(format!(
                "{}: {}",
                t!("window.settings.keybinds.conflict"),
                t!(conflicting_label_key.as_str())
            ));
            cx.notify();
            return;
        }

        crate::keybinds::set_binding(&action_id, &ks, cx);
        self.recording_action = None;
        self.keybind_error = None;
        cx.notify();
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let selected_idx = SettingsTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);

        let content: AnyElement = match self.tab {
            SettingsTab::General => self.render_general_pane(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_pane(cx).into_any_element(),
            SettingsTab::Panels => self.render_panels_pane(cx).into_any_element(),
            SettingsTab::Connection => self.render_connection_pane(cx).into_any_element(),
            // The AI tab is either its landing page or one of its
            // second-level pages.
            SettingsTab::Ai => match self.ai_sub_page {
                Some(sub_page) => self.render_ai_sub_page(sub_page, cx).into_any_element(),
                None => self.render_ai_pane(cx).into_any_element(),
            },
            SettingsTab::Keybinds => self.render_keybinds_pane(cx).into_any_element(),
        };

        // The content pane is overlapped at the top by the `h_11` (44px)
        // client-side window controls strip on Windows/Linux. Push content
        // down by that height so the first section header isn't hidden under
        // the buttons. macOS uses the native title bar and renders no
        // overlap, so no padding is added there.
        let content = if HAS_CLIENT_CONTROLS {
            div().pt_6().h_full().child(content).into_any_element()
        } else {
            content
        };

        render_sidebar_window(
            render_tab_sidebar(
                SettingsTab::sidebar_entries(),
                px(180.0),
                selected_idx,
                move |idx, _w, cx| {
                    handle.update(cx, |view, _| {
                        view.tab = SettingsTab::ALL[idx];
                        // Switching tabs always lands on the tab's own page,
                        // never on a sub-page opened earlier.
                        view.ai_sub_page = None;
                    });
                },
            ),
            content,
        )
        .id("settings-root")
        .track_focus(&self.focus_handle)
        // Intercept key presses while recording a keybind.
        .when(self.recording_action.is_some(), |el| {
            el.on_key_down(cx.listener(Self::on_key_recording))
        })
        // Escape leaves a second-level page. Nothing else is intercepted: the
        // sub-pages hold ordinary controls, and the back arrow in the page
        // header is the primary way out.
        .when(
            self.tab == SettingsTab::Ai && self.ai_sub_page.is_some(),
            |el| {
                el.on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                    if event.keystroke.key.as_str() == "escape" && this.ai_sub_page.take().is_some()
                    {
                        cx.notify();
                    }
                }))
            },
        )
        .when(HAS_CLIENT_CONTROLS, |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_11()
                    .flex()
                    .items_center()
                    .pr_2()
                    .child(WindowControls::new("settings")),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// open_path helper — best-effort cross-platform "reveal in Finder/Explorer"
// ---------------------------------------------------------------------------

/// Build the list of font family names shown in the Terminal section's
/// font dropdown.
///
/// We query the OS for **every** installed family (via the gpui text
/// system) so the user can pick any font — not just ones our heuristic
/// flagged as monospace. The platform default family is always prepended
/// so a fresh install shows a sensible first option, and the currently
/// configured family is appended if it isn't already in the list (so a
/// hand-edited `config.toml` value stays visible/selectable).
fn collect_monospace_fonts(cx: &mut App) -> Vec<String> {
    let mut names: Vec<String> = cx.text_system().all_font_names();

    // De-dup while preserving order.
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.to_lowercase()));

    // Ensure the platform default is present and first.
    let default_family = crabport_core::config::default_terminal_font_family().to_string();
    names.retain(|n| *n != default_family);
    let mut result = vec![default_family];
    result.extend(names);

    // Ensure the currently configured family is selectable even if it's a
    // custom value that `all_font_names` didn't return (e.g. a family name
    // from a hand-edited config.toml that the OS doesn't report).
    let configured = crabport_core::config::snapshot()
        .appearance
        .terminal
        .effective_font_family()
        .to_string();
    if !result.contains(&configured) {
        result.push(configured);
    }

    result
}

fn open_path(path: &std::path::Path, _cx: &mut App) -> Result<(), ()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|_| ())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|_| ())?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|_| ())?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err(())
}
