//! Theme Editor window.
//!
//! Lets the user build a custom theme directly in the GUI instead of
//! hand-editing a `.toml` file. The editor:
//!
//! - seeds a **working copy** from the currently applied theme
//!   (`config.toml`'s `[appearance.theme]`),
//! - shows every color leaf of [`ThemeConfig`] grouped by its TOML table
//!   (base / button / terminal / …), one hex input + live swatch per field,
//! - **live-previews** each valid edit process-wide via
//!   [`crate::color::preview_theme`] (nothing is persisted while typing),
//! - on **Save**, writes the working copy to
//!   `{data_dir}/crabport/themes/<id>.toml` via
//!   [`crate::theme::save_custom_theme`] and applies it as the selected
//!   theme (persisted to `config.toml`),
//! - on close without saving, the `cx.on_release` hook restores the
//!   persisted theme so the preview never outlives the window.
//!
//! Field names are shown as their raw TOML keys (`bg_hover`, `border_focused`,
//! …) on purpose: they match what a user would edit in the file, so the GUI
//! doubles as documentation for the file format. An empty field inherits the
//! modern-dark default (same rule as a missing key in a theme file).

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use rust_i18n::t;

use crabport_core::config::{self, ThemeConfig};

use crate::color::*;
use crate::components::button::Button;
use crate::components::input::StyledInput;
use crate::components::settings_section::Section;
use crate::components::window_controls::{HAS_CLIENT_CONTROLS, WindowControls};
use crate::components::window_layout::{
    SidebarTabEntry, render_sidebar_window, render_tab_sidebar,
};

// ---------------------------------------------------------------------------
// Field registry
// ---------------------------------------------------------------------------

/// One editable color leaf of [`ThemeConfig`]: its TOML table (`group`),
/// key (`field`), and accessors into the config struct.
struct FieldDef {
    group: &'static str,
    field: &'static str,
    get: fn(&ThemeConfig) -> &String,
    get_mut: fn(&mut ThemeConfig) -> &mut String,
}

/// Every color leaf of [`ThemeConfig`], in the same group/field order as the
/// struct definition (and the theme TOML files). The editor's inputs are
/// index-parallel to this list.
fn theme_fields() -> Vec<FieldDef> {
    macro_rules! group {
        ($out:ident; $g:ident: $($f:ident),+ $(,)?) => {
            $(
                $out.push(FieldDef {
                    group: stringify!($g),
                    field: stringify!($f),
                    get: |c: &ThemeConfig| &c.$g.$f,
                    get_mut: |c: &mut ThemeConfig| &mut c.$g.$f,
                });
            )+
        };
    }
    let mut v: Vec<FieldDef> = Vec::new();
    group!(v; base: bg_base, bg_sidebar, bg_tab_bar);
    group!(v; border: border);
    group!(v; surface: surface_hover, surface_active);
    group!(v; text: text_primary, text_muted);
    group!(v; button: bg, bg_hover, bg_selected, bg_pressed, bg_disabled, border, text_disabled);
    group!(v; button_primary: bg, bg_hover, bg_selected, bg_pressed, bg_disabled, border, text_disabled);
    group!(v; button_ghost: bg, bg_hover, bg_selected, bg_pressed, bg_disabled, border, text_disabled);
    group!(v; tab_button: bg, bg_hover, bg_selected, bg_pressed, bg_disabled, border, text_disabled);
    group!(v; input: bg, bg_focused, bg_disabled, text_disabled, border, border_hover, border_focused, border_error, placeholder, selection);
    group!(v; command: overlay, bg, border, item_hover, item_active, group_label);
    group!(v; terminal: fg, bg, cursor, black, red, green, yellow, blue, magenta, cyan, white, bright_black, bright_red, bright_green, bright_yellow, bright_blue, bright_magenta, bright_cyan, bright_white);
    group!(v; selection: bg);
    v
}

/// Ordered unique group keys, derived from [`theme_fields`].
fn theme_groups() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for def in theme_fields() {
        if groups.last() != Some(&def.group) {
            groups.push(def.group);
        }
    }
    groups
}

/// Sidebar label for a group key (translated; the raw key stays visible in
/// the section description so it still maps back to the TOML table name).
fn group_label(group: &str) -> String {
    let key = format!("window.theme_editor.group.{group}");
    t!(&key).to_string()
}

// ---------------------------------------------------------------------------
// Root view
// ---------------------------------------------------------------------------

/// Root view for the Theme Editor window.
pub struct ThemeEditorWindow {
    /// Selected group in the sidebar (index into [`theme_groups`]).
    group_idx: usize,
    /// Theme name / id input. Pre-filled with the current theme's name; the
    /// sanitized value becomes the saved file's stem and catalog id.
    name_input: Entity<InputState>,
    name_focused: bool,
    /// One hex input per [`theme_fields`] entry, index-parallel.
    color_inputs: Vec<Entity<InputState>>,
    /// Which color input (index) currently has keyboard focus, if any.
    focused_field: Option<usize>,
    /// The theme being edited. Live-previewed on every change; only written
    /// to disk/config on Save.
    working: ThemeConfig,
    /// Feedback line under the header: `(is_error, message)`.
    status: Option<(bool, String)>,
}

impl ThemeEditorWindow {
    /// Open the Theme Editor window (or no-op if one already exists —
    /// callers should normally go through [`crate::windows::focus_or_open`]
    /// for the singleton check).
    pub fn open(cx: &mut App) -> WindowHandle<gpui_component::Root> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(760.0), px(820.0)), cx)),
            // See `app::open_main_window` for the per-platform titlebar
            // rationale (same setup as Settings/About).
            titlebar: Some(TitlebarOptions {
                title: Some(t!("window.theme_editor.title").to_string().into()),
                appears_transparent: true,
                #[cfg(target_os = "macos")]
                traffic_light_position: Some(point(px(12.0), px(14.0))),
                ..Default::default()
            }),
            #[cfg(target_os = "macos")]
            window_background: WindowBackgroundAppearance::Blurred,
            #[cfg(target_os = "linux")]
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(Size {
                width: px(600.0),
                height: px(480.0),
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let view = cx.new(|cx| ThemeEditorWindow::new(window, cx));
                gpui_component::Root::new(view, window, cx)
            })
        })
        .expect("Failed to open Theme Editor window")
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Seed the working copy from the theme that's currently applied so
        // the editor opens as "tweak what I'm looking at".
        let working = config::snapshot().appearance.theme.clone();

        let name_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx)
                .placeholder(t!("window.theme_editor.name_placeholder").to_string());
            s.set_value(working.name.clone(), window, cx);
            s
        });
        cx.subscribe(
            &name_input,
            |this, _input, event: &InputEvent, cx| match event {
                InputEvent::Focus => {
                    this.name_focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.name_focused = false;
                    cx.notify();
                }
                InputEvent::Change { .. } => {
                    this.status = None;
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();

        // One input per color leaf, each wired to write straight back into
        // `working` and live-preview the result.
        let defs = theme_fields();
        let mut color_inputs = Vec::with_capacity(defs.len());
        for (i, def) in defs.iter().enumerate() {
            let value = (def.get)(&working).clone();
            let input = cx.new(|cx| {
                let mut s = InputState::new(window, cx)
                    .placeholder(t!("window.theme_editor.color_placeholder").to_string());
                s.set_value(value, window, cx);
                s
            });
            let get_mut = def.get_mut;
            cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                match event {
                    InputEvent::Change { .. } => {
                        let raw = input.read(cx).value().to_string();
                        *get_mut(&mut this.working) = raw;
                        this.status = None;
                        // Live preview: parse failures inside `from_config`
                        // fall back per-field to modern-dark, so previewing a
                        // half-typed hex is always safe.
                        preview_theme(&this.working);
                        cx.refresh_windows();
                    }
                    InputEvent::Focus => {
                        this.focused_field = Some(i);
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        if this.focused_field == Some(i) {
                            this.focused_field = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            })
            .detach();
            color_inputs.push(input);
        }

        // When the window goes away (saved or not), drop the preview and
        // restore whatever `config.toml` holds. After a Save that's the
        // just-applied custom theme, so this is a no-op visually.
        cx.on_release(|_, cx| {
            crate::refresh_theme_with(cx);
        })
        .detach();

        Self {
            group_idx: 0,
            name_input,
            name_focused: false,
            color_inputs,
            focused_field: None,
            working,
            status: None,
        }
    }

    /// Turn the raw name-input text into a filesystem/catalog-safe id.
    /// Alphanumerics (incl. CJK), `-` and `_` pass through; everything else
    /// becomes `-`. Returns `None` when nothing usable remains.
    fn sanitize_id(raw: &str) -> Option<String> {
        let s: String = raw
            .trim()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let s = s.trim_matches('-').to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    /// Save the working copy as a custom theme and apply it.
    fn save(&mut self, cx: &mut Context<Self>) {
        let raw = self.name_input.read(cx).value().to_string();
        let Some(id) = Self::sanitize_id(&raw) else {
            self.status = Some((true, t!("window.theme_editor.invalid_name").to_string()));
            cx.notify();
            return;
        };
        self.working.name = id.clone();
        match crate::theme::save_custom_theme(&self.working) {
            Ok(_path) => {
                // Persist the selection so the saved theme survives restarts
                // (and so the on-close restore keeps showing it).
                apply_theme(&id);
                crate::refresh_theme_with(cx);
                self.status = Some((false, t!("window.theme_editor.saved").to_string()));
            }
            Err(e) => {
                self.status = Some((
                    true,
                    format!("{}: {e}", t!("window.theme_editor.save_failed")),
                ));
            }
        }
        cx.notify();
    }

    /// Discard edits: re-seed the working copy from the persisted theme and
    /// push the old values back into every input.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.working = config::snapshot().appearance.theme.clone();
        let name = self.working.name.clone();
        self.name_input.update(cx, |s, cx| {
            s.set_value(name, window, cx);
        });
        let defs = theme_fields();
        for (def, input) in defs.iter().zip(&self.color_inputs) {
            let value = (def.get)(&self.working).clone();
            input.update(cx, |s, cx| {
                s.set_value(value, window, cx);
            });
        }
        // `set_value` fires Change events that already re-preview field by
        // field; end on a full refresh so the restore is atomic-looking.
        preview_theme(&self.working);
        self.status = None;
        cx.refresh_windows();
        cx.notify();
    }

    /// Small rounded square filled with the parsed color (transparent when
    /// the value is empty/invalid). 6-digit values route through `rgb()`,
    /// 8-digit through `rgba()` — same convention as the theme parser.
    fn swatch(value: &str) -> Div {
        let base = div()
            .w(px(14.0))
            .h(px(14.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(border()));
        let trimmed = value.trim().trim_start_matches('#');
        match (trimmed.len(), u32::from_str_radix(trimmed, 16)) {
            (6, Ok(v)) => base.bg(rgb(v)),
            (8, Ok(v)) => base.bg(rgba(v)),
            _ => base,
        }
    }

    /// Header row: theme name + Save / Reset + status line.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let save_handle = handle.clone();
        let reset_handle = handle;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap_3()
                    // Buttons inherit text size from their container; without
                    // this they render at the window default and look
                    // oversized next to the inputs (same fix as dialog.rs).
                    .text_sm()
                    .child(
                        div().flex_1().child(
                            StyledInput::new("theme-editor-name", self.name_input.clone())
                                .label(t!("window.theme_editor.name_label").to_string())
                                .focused(self.name_focused),
                        ),
                    )
                    .child(
                        Button::new("theme-editor-save")
                            .primary()
                            .w_auto()
                            .centered(true)
                            .child(t!("window.theme_editor.save").to_string())
                            .on_click(move |_e, _w, cx| {
                                save_handle.update(cx, |view, cx| view.save(cx));
                            }),
                    )
                    .child(
                        Button::new("theme-editor-reset")
                            .w_auto()
                            .centered(true)
                            .child(t!("window.theme_editor.reset").to_string())
                            .on_click(move |_e, w, cx| {
                                reset_handle.update(cx, |view, cx| view.reset(w, cx));
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(t!("window.theme_editor.desc").to_string()),
            )
            .when_some(self.status.clone(), |el, (is_error, msg)| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(if is_error {
                            input_border_error()
                        } else {
                            text_muted()
                        }))
                        .child(msg),
                )
            })
    }

    /// Field list for the currently selected group.
    fn render_group_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = theme_groups();
        let group = groups.get(self.group_idx).copied().unwrap_or("base");
        let defs = theme_fields();

        let mut section = Section::new()
            .header(group_label(group))
            // Show the raw TOML table name so the GUI maps back to the file
            // format (`[terminal]`, `[button_primary]`, …).
            .desc(format!("[{group}]"));

        for (i, def) in defs.iter().enumerate() {
            if def.group != group {
                continue;
            }
            let value = (def.get)(&self.working).clone();
            let invalid = !value.trim().is_empty() && parse_hex(&value).is_none();
            let mut input = StyledInput::new(
                format!("theme-field-{}-{}", def.group, def.field),
                self.color_inputs[i].clone(),
            )
            .focused(self.focused_field == Some(i))
            .prefix(Self::swatch(&value));
            if invalid {
                input = input.error(t!("window.theme_editor.invalid_color").to_string());
            }
            section = section.field(def.field.to_string(), div().w(px(220.0)).child(input));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .gap_4()
            .child(self.render_header(cx))
            .child(
                div()
                    .id("theme-editor-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(section),
            )
    }
}

impl Render for ThemeEditorWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let groups = theme_groups();
        let entries: Vec<SidebarTabEntry> = groups
            .iter()
            .map(|g| SidebarTabEntry {
                id: ElementId::Name(format!("theme-editor-group-{g}").into()),
                label: group_label(g).into(),
                icon: None,
            })
            .collect();

        let content: AnyElement = self.render_group_pane(cx).into_any_element();

        // Same client-controls top offset as Settings/About: on Windows and
        // Linux the 44px window-controls strip overlaps the content pane.
        let content = if HAS_CLIENT_CONTROLS {
            div().pt_6().h_full().child(content).into_any_element()
        } else {
            content
        };

        render_sidebar_window(
            render_tab_sidebar(entries, px(170.0), self.group_idx, move |idx, _w, cx| {
                handle.update(cx, |view, cx| {
                    view.group_idx = idx;
                    cx.notify();
                });
            }),
            content,
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
                    .child(WindowControls::new("theme-editor")),
            )
        })
    }
}
