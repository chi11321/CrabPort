//! Settings window.
//!
//! Renders a sidebar (General / Appearance) on the left and a scrollable
//! content pane on the right. Every control reads from and writes to the
//! process-wide [`crabport_core::config::CONFIG`] `LazyLock`, so changes are
//! persisted to `config.toml` immediately and visible to every other window
//! in the process.
//!
//! Sections are built declaratively via [`crate::windows::settings_section::Section`].

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::label::Label;
use rust_i18n::t;

use crabport_core::config;

use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::number_input::{StyledNumberInput, subscribe_number_filter};
use crate::components::settings_section::Section;
use crate::components::window_controls::{HAS_CLIENT_CONTROLS, WindowControls};
use crate::components::window_layout::{
    SidebarTabEntry, render_sidebar_window, render_tab_sidebar,
};

// ---------------------------------------------------------------------------
// Tab enum
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    General,
    Appearance,
    Keybinds,
}

impl SettingsTab {
    const ALL: [SettingsTab; 3] = [
        SettingsTab::General,
        SettingsTab::Appearance,
        SettingsTab::Keybinds,
    ];

    fn label(self) -> SharedString {
        match self {
            SettingsTab::General => t!("window.settings.tab.general").into(),
            SettingsTab::Appearance => t!("window.settings.tab.appearance").into(),
            SettingsTab::Keybinds => t!("window.settings.tab.keybinds").into(),
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

// ---------------------------------------------------------------------------
// Root view
// ---------------------------------------------------------------------------

/// Root view for the Settings window.
pub struct SettingsWindow {
    tab: SettingsTab,
    // Dropdown open states (Dropdown is uncontrolled — caller manages it).
    locale_dropdown_open: bool,
    theme_dropdown_open: bool,
    font_family_dropdown_open: bool,
    /// Search input backing the terminal font-family dropdown. Lets the
    /// user type to filter the (potentially long) list of installed fonts.
    font_search_input: Entity<InputState>,
    /// `InputState` backing the terminal font-size stepper. Pre-filled with
    /// the persisted size on open and re-clamped on every edit via
    /// [`subscribe_number_filter`].
    font_size_input: Entity<InputState>,
    /// Focus flag for the font-size input (drives the accent border).
    font_size_focused: bool,
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

impl SettingsWindow {
    /// Open the Settings window (or no-op if one already exists — callers
    /// should normally go through [`crate::windows::focus_or_open`] for the
    /// singleton check).
    pub fn open(cx: &mut App) -> WindowHandle<gpui_component::Root> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(720.0), px(820.0)), cx)),
            #[cfg(target_os = "macos")]
            titlebar: Some(TitlebarOptions {
                title: Some(t!("window.settings.title").to_string().into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(14.0))),
                ..Default::default()
            }),
            #[cfg(target_os = "linux")]
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(Size {
                width: px(560.0),
                height: px(440.0),
            }),
            ..Default::default()
        };

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
        // Search box for the font-family dropdown — filters the list of
        // installed fonts by case-insensitive substring.
        let font_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("groups.search_placeholder").to_string())
        });
        Self {
            tab: SettingsTab::General,
            locale_dropdown_open: false,
            theme_dropdown_open: false,
            font_family_dropdown_open: false,
            font_search_input,
            font_size_input,
            font_size_focused: false,
            mono_font_names: Vec::new(),
            recording_action: None,
            focus_handle: cx.focus_handle(),
            keybind_error: None,
        }
    }

    // -------------------------------------------------------------------
    // General pane (declarative sections)
    // -------------------------------------------------------------------

    fn render_general_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store_path = crabport_core::store::default_data_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string());
        let handle = cx.entity().clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .gap_6()
            // --- Data directory section ---
            .child(
                Section::new()
                    .header(t!("window.settings.general.section_data"))
                    .desc(t!("window.settings.general.open_data_dir_desc"))
                    .bare(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(Label::new(store_path)),
                    )
                    .bare(
                        Button::new("settings-open-data-dir")
                            .child(t!("window.settings.general.open_data_dir").to_string())
                            .w_auto()
                            .centered(true)
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
                        Button::new("settings-reset-config")
                            .child(t!("window.settings.general.reset_config").to_string())
                            .w_auto()
                            .centered(true)
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
            )
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

        // Map the persisted theme preset name to a dropdown index. Unknown
        // names (e.g. a hand-edited config.toml) fall back to the default.
        let presets = crabport_core::config::ThemeConfig::PRESETS;
        let current_name = config::snapshot().appearance.theme.name;
        let theme_idx = presets
            .iter()
            .position(|p| *p == current_name.as_str())
            .unwrap_or(0);

        // Lazily build the font family list on first render.
        if self.mono_font_names.is_empty() {
            self.mono_font_names = collect_monospace_fonts(cx);
        }
        let mono_fonts = self.mono_font_names.clone();

        // --- Language dropdown ---
        let locale_dropdown = {
            let h = handle.clone();
            Dropdown::new("settings-locale")
                .item(t!("window.settings.appearance.language_en"))
                .item(t!("window.settings.appearance.language_zh_cn"))
                .selected(locale_idx)
                .is_open(self.locale_dropdown_open)
                .on_toggle(move |_w, cx| {
                    h.update(cx, |view, cx| {
                        view.locale_dropdown_open = !view.locale_dropdown_open;
                        cx.notify();
                    });
                })
                .on_change(move |idx, _w, cx| {
                    let locale = if idx == 1 { "zh-CN" } else { "en" };
                    let _ = config::update(|cfg| {
                        cfg.appearance.locale = locale.to_string();
                    });
                    crate::set_locale(locale);
                    cx.refresh_windows();
                })
        };

        // --- Theme dropdown ---
        let theme_dropdown = {
            let h_for_toggle = handle.clone();
            let h_for_change = handle.clone();
            Dropdown::new("settings-theme")
                .item(t!("window.settings.appearance.theme_modern_dark"))
                .item(t!("window.settings.appearance.theme_mocha"))
                .item(t!("window.settings.appearance.theme_tokyo_night"))
                .selected(theme_idx)
                .is_open(self.theme_dropdown_open)
                .on_toggle(move |_w, cx| {
                    h_for_toggle.update(cx, |view, cx| {
                        view.theme_dropdown_open = !view.theme_dropdown_open;
                        cx.notify();
                    });
                })
                .on_change(move |idx, _w, cx| {
                    let id = presets.get(idx).copied().unwrap_or("modern-dark");
                    let _ = config::update(|cfg| {
                        cfg.appearance.theme = crabport_core::config::ThemeConfig::preset(id);
                    });
                    crate::refresh_theme_with(cx);
                    h_for_change.update(cx, |view, cx| {
                        view.theme_dropdown_open = false;
                        cx.notify();
                    });
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

        // Build the pane from declarative sections.
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
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
                    .bare(div().w(px(240.0)).child(theme_dropdown)),
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
                // Clickable Kbd chip (left-click = rebind, right-click = clear)
                .child(
                    div()
                        .id(SharedString::from(format!("keybind-chip-{action_id}")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .min_w(px(72.0))
                        .h(px(28.0))
                        .px_3()
                        .rounded_md()
                        .cursor_pointer()
                        .border_1()
                        .border_color(rgb(if is_recording {
                            text_primary()
                        } else {
                            border()
                        }))
                        .bg(rgb(if is_recording {
                            surface_active()
                        } else {
                            surface_hover()
                        }))
                        .hover(|s| {
                            s.bg(rgb(if is_recording {
                                surface_active()
                            } else {
                                border()
                            }))
                        })
                        .child(chip_child)
                        // Left-click: start recording a new binding
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
                        })
                        // Right-click: clear binding (set to empty = disabled)
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
                        }),
                );

            section = section.bare(row);
        }

        let reset_all = Section::new()
            .header(t!("window.settings.keybinds.reset_all"))
            .desc(t!("window.settings.keybinds.reset_all_desc"))
            .bare(
                Button::new("settings-reset-all-keybinds")
                    .child(t!("window.settings.keybinds.reset_all").to_string())
                    .w_auto()
                    .centered(true)
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
            SettingsTab::Keybinds => self.render_keybinds_pane(cx).into_any_element(),
        };

        render_sidebar_window(
            render_tab_sidebar(
                SettingsTab::sidebar_entries(),
                px(180.0),
                selected_idx,
                move |idx, _w, cx| {
                    handle.update(cx, |view, _| {
                        view.tab = SettingsTab::ALL[idx];
                    });
                },
            ),
            content,
        )
        .track_focus(&self.focus_handle)
        // Intercept key presses while recording a keybind.
        .when(self.recording_action.is_some(), |el| {
            el.on_key_down(cx.listener(Self::on_key_recording))
        })
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
