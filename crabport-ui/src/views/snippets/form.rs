//! Snippet form dialog (create / edit).
//!
//! Mirrors the overlay-dialog pattern used by `TunnelFormState` /
//! `TunnelFormView` in `crabport-ui/src/views/tunnels/form.rs`:
//! - `SnippetFormState` is owned by `CrabportApp` and holds `Entity<InputState>`
//!   fields plus open/close animation state.
//! - `SnippetFormView` is a pure `RenderOnce` renderer that reads a snapshot of
//!   the state and emits an absolute overlay + centered dialog.
//!
//! The view does NOT persist anything itself — it reads its inputs, packages
//! them into a `SnippetFormOutput`, and invokes the `on_save` callback. The
//! caller (`CrabportApp`) is responsible for store CRUD.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind};

use crate::app::CrabportApp;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::input::StyledInput;
use crate::components::overlay::render_overlay;
use crate::motion::{DURATION_BASE, EASE_STANDARD, RADIUS_LG};

// ---------------------------------------------------------------------------
// Output passed to the save callback
// ---------------------------------------------------------------------------

/// Parsed form values handed to the `on_save` callback. The caller resolves
/// `editing_id` against the store (UPDATE if `Some`, INSERT if `None`).
#[derive(Clone, Debug)]
pub struct SnippetFormOutput {
    pub editing_id: Option<i64>,
    pub name: String,
    pub command: String,
    /// Starred by the user to pin it above un-starred snippets.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped.
    pub group_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// SnippetValidationErrors — per-field error strings shown via StyledInput.error()
// ---------------------------------------------------------------------------

/// Per-field validation errors for the snippet form. A field is `Some` when it
/// has an error to display; `None` means it passed validation.
#[derive(Clone, Default)]
pub struct SnippetValidationErrors {
    pub name: Option<SharedString>,
    pub command: Option<SharedString>,
}

impl SnippetValidationErrors {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.command.is_none()
    }
}

// ---------------------------------------------------------------------------
// SnippetFormState — owned by CrabportApp
// ---------------------------------------------------------------------------

/// Holds all mutable state for the snippet form overlay so that
/// `SnippetFormView` can be a pure `RenderOnce` renderer.
#[derive(Clone)]
pub struct SnippetFormState {
    /// `Some(id)` when editing an existing snippet; `None` when creating.
    pub editing_id: Option<i64>,
    pub name_input: Entity<InputState>,
    /// Created with `.multi_line(true)` so the underlying InputState is a
    /// textarea. The `StyledInput` also receives `.multi_line(true).rows(5)`
    /// at render time to size the shell.
    pub command_input: Entity<InputState>,
    // Focus states (mirrors TunnelFormState)
    pub name_focused: bool,
    pub command_focused: bool,
    /// Open/close animation state. `true` while the overlay is visible
    /// (drives the backdrop fade + dialog slide-in transition).
    pub open: bool,
    /// Per-field validation errors. Populated by `validate()` and rendered
    /// via `StyledInput.error(...)` on the relevant fields. Cleared on open.
    pub errors: SnippetValidationErrors,
    /// Starred flag, edited via a star toggle in the form.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped. Edited via a group
    /// dropdown in the form.
    pub group_id: Option<i64>,
    /// Open state for the group dropdown. Owned here so the renderer is a
    /// pure function of the state (mirrors `host_dropdown_open` on the tunnel
    /// form).
    pub group_dropdown_open: bool,
    /// Search input for the group dropdown (filtering + create).
    pub group_search_input: Entity<InputState>,
    pub on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub on_save: Option<Rc<dyn Fn(SnippetFormOutput, &mut Window, &mut App) + 'static>>,
}

impl SnippetFormState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let command_input = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        let group_search_input = cx.new(|cx| InputState::new(window, cx));

        Self {
            editing_id: None,
            name_input,
            command_input,
            name_focused: false,
            command_focused: false,
            open: false,
            errors: SnippetValidationErrors::default(),
            favorite: false,
            group_id: None,
            group_dropdown_open: false,
            group_search_input,
            on_close: None,
            on_save: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.open = true;
        self.errors = SnippetValidationErrors::default();
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub fn close(&mut self) {
        self.open = false;
        self.group_dropdown_open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Reset all fields to blank defaults and open the dialog in create mode.
    pub fn open_for_create(&mut self, window: &mut Window, cx: &mut App) {
        self.editing_id = None;
        self.favorite = false;
        self.group_id = None;
        self.group_dropdown_open = false;
        self.group_search_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.name_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.command_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.open(window, cx);
    }

    /// Populate the fields from an existing snippet, set `editing_id`,
    /// and open the dialog in edit mode.
    pub fn open_for_edit(
        &mut self,
        id: i64,
        name: &str,
        command: &str,
        favorite: bool,
        group_id: Option<i64>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let name = name.to_string();
        let command = command.to_string();
        self.editing_id = Some(id);
        self.favorite = favorite;
        self.group_id = group_id;
        self.group_dropdown_open = false;
        self.group_search_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.name_input
            .update(cx, |state, cx| state.set_value(&name, window, cx));
        self.command_input
            .update(cx, |state, cx| state.set_value(&command, window, cx));
        self.open(window, cx);
    }

    pub fn name_text(&self, cx: &App) -> String {
        self.name_input.read(cx).text().to_string()
    }

    pub fn command_text(&self, cx: &App) -> String {
        self.command_input.read(cx).text().to_string()
    }

    /// Build a `SnippetFormOutput` from the current form state.
    pub fn output(&self, cx: &App) -> SnippetFormOutput {
        SnippetFormOutput {
            editing_id: self.editing_id,
            name: self.name_text(cx),
            command: self.command_text(cx),
            favorite: self.favorite,
            group_id: self.group_id,
        }
    }

    /// Validate the form against the required-field rules. Populates
    /// `self.errors` and returns `true` if the form is valid (no errors).
    ///
    /// Rules:
    /// - Name is required (non-empty after trim).
    /// - Command is required (non-empty after trim).
    pub fn validate(&mut self, cx: &App) -> bool {
        let mut errors = SnippetValidationErrors::default();

        if self.name_text(cx).trim().is_empty() {
            errors.name = Some(t!("snippets.error_name_required").into());
        }

        if self.command_text(cx).trim().is_empty() {
            errors.command = Some(t!("snippets.error_command_required").into());
        }

        let ok = errors.is_empty();
        self.errors = errors;
        ok
    }
}

// ---------------------------------------------------------------------------
// SnippetFormView — pure RenderOnce renderer
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct SnippetFormView {
    open: bool,
    editing: bool,
    name_input: Entity<InputState>,
    command_input: Entity<InputState>,
    name_focused: bool,
    command_focused: bool,
    errors: SnippetValidationErrors,
    group_id: Option<i64>,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(SnippetFormOutput, &mut Window, &mut App) + 'static>>,
}

impl SnippetFormView {
    pub fn new(state: &SnippetFormState, app: Entity<CrabportApp>) -> Self {
        Self {
            open: state.open,
            editing: state.editing_id.is_some(),
            name_input: state.name_input.clone(),
            command_input: state.command_input.clone(),
            name_focused: state.name_focused,
            command_focused: state.command_focused,
            errors: state.errors.clone(),
            group_id: state.group_id,
            group_dropdown_open: state.group_dropdown_open,
            group_search_input: state.group_search_input.clone(),
            app,
            on_close: state.on_close.clone(),
            on_save: state.on_save.clone(),
        }
    }
}

impl RenderOnce for SnippetFormView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();

        // Load snippet groups from the store so newly-created groups appear
        // without a round-trip through the app state (mirrors how the tunnel
        // form receives `hosts` pre-loaded).
        let groups = AppState::store(_cx)
            .lock()
            .groups(GroupKind::Snippet)
            .unwrap_or_default();

        render_overlay(
            ElementId::Name("snippet-edit-overlay".into()),
            self.open,
            self.on_close,
            render_dialog(
                self.open,
                self.editing,
                self.name_input,
                self.command_input,
                self.name_focused,
                self.command_focused,
                self.errors,
                self.group_id,
                self.group_dropdown_open,
                self.group_search_input,
                groups,
                self.app,
                on_close_for_dialog,
                self.on_save,
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_dialog(
    open: bool,
    editing: bool,
    name_input: Entity<InputState>,
    command_input: Entity<InputState>,
    name_focused: bool,
    command_focused: bool,
    errors: SnippetValidationErrors,
    group_id: Option<i64>,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    groups: Vec<GroupEntry>,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(SnippetFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("snippet-edit-dialog".into());

    let title = if editing {
        t!("snippets.edit_title").to_string()
    } else {
        t!("snippets.new_button").to_string()
    };

    div()
        .id(dialog_id.clone())
        .w(px(420.0))
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded(RADIUS_LG)
        .shadow_lg()
        .flex()
        .flex_col()
        .p_6()
        .gap_4()
        .opacity(0.0)
        .mt(px(-16.0))
        .when(open, |el| {
            el.on_click(|_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            open,
            DURATION_BASE,
            EASE_STANDARD,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // Title
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(text_primary()))
                .child(title),
        )
        // Name
        .child(
            StyledInput::new("snippet-edit-name", name_input)
                .label(t!("snippets.name").to_string())
                .focused(name_focused)
                .when_some(errors.name.clone(), |el, e| el.error(e)),
        )
        // Group dropdown
        .child(render_group_selector(
            group_id,
            group_dropdown_open,
            groups,
            group_search_input,
            app.clone(),
        ))
        // Command (multi-line)
        .child(
            div().child(
                StyledInput::new("snippet-edit-command", command_input)
                    .label(t!("snippets.command").to_string())
                    .multi_line(true)
                    .rows(5)
                    .focused(command_focused)
                    .when_some(errors.command.clone(), |el, e| el.error(e)),
            ),
        )
        // Buttons
        .child(render_buttons(app, on_close, on_save))
}

fn render_buttons(
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(SnippetFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("snippet-edit-overlay".into());
    let dialog_id = ElementId::Name("snippet-edit-dialog".into());

    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new("snippet-edit-cancel")
                .centered(true)
                .child(t!("snippets.cancel").to_string())
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new("snippet-edit-save")
                .primary()
                .centered(true)
                .child(t!("snippets.save").to_string())
                .on_click(move |_e, w, cx| {
                    // Reset the overlay/dialog transitions so the next open
                    // starts fresh (mirrors tunnel form's save button).
                    gpui_animation::reset_transition(&overlay_id);
                    gpui_animation::reset_transition(&dialog_id);
                    // Validate required fields before building the output. If
                    // invalid, per-field errors are shown and the save flow is
                    // aborted (no toast — per-field errors are sufficient).
                    let output: Option<SnippetFormOutput> = app.update(cx, |app, cx| {
                        let valid = app
                            .snippet_form
                            .as_mut()
                            .map(|form| form.validate(cx))
                            .unwrap_or(true);
                        if !valid {
                            cx.notify();
                            return None;
                        }
                        app.snippet_form.as_ref().map(|form| form.output(cx))
                    });
                    if let Some(out) = output {
                        if let Some(ref cb) = on_save {
                            cb(out, w, cx);
                        }
                    }
                }),
        )
}

// ---------------------------------------------------------------------------
// Group dropdown
// ---------------------------------------------------------------------------

/// Group dropdown. Items = `[connection_form.group_none] ++ groups`. Mirrors
/// `render_host_selector` in the tunnel form.
fn render_group_selector(
    group_id: Option<i64>,
    dropdown_open: bool,
    groups: Vec<GroupEntry>,
    group_search_input: Entity<InputState>,
    app: Entity<CrabportApp>,
) -> impl IntoElement {
    let label_div = div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(t!("tunnel_form.group").to_string());

    // Index 0 = "None" (ungrouped); following indices map to groups[i-1].
    let selected_idx =
        group_id.and_then(|id| groups.iter().position(|g| g.id == id).map(|i| i + 1));

    let mut dropdown = Dropdown::new("snippet-group-dropdown")
        .placeholder(t!("tunnel_form.group").to_string())
        .is_open(dropdown_open)
        .searchable(group_search_input)
        .on_create({
            let app = app.clone();
            move |name, _w, cx| {
                app.update(cx, |app, cx| {
                    let kind = crabport_core::credential::GroupKind::Snippet;
                    if let Ok(gid) = crate::app_state::AppState::store(cx)
                        .lock()
                        .add_group(&name, kind, None)
                    {
                        if let Some(ref mut form) = app.snippet_form {
                            form.group_id = Some(gid);
                            form.group_dropdown_open = false;
                            cx.notify();
                        }
                    }
                });
            }
        })
        .on_toggle({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.snippet_form {
                        form.group_dropdown_open = !form.group_dropdown_open;
                        cx.notify();
                    }
                });
            }
        })
        .on_change({
            let app = app.clone();
            let groups = groups.clone();
            move |index, _w, cx| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.snippet_form {
                        form.group_id = if index == 0 {
                            None
                        } else {
                            groups.get(index - 1).map(|g| g.id)
                        };
                        form.group_dropdown_open = false;
                        cx.notify();
                    }
                });
            }
        });

    dropdown = dropdown.item_with_value(t!("tunnel_form.group_none").to_string(), "");
    for g in &groups {
        dropdown = dropdown.item_with_value(g.name.clone(), g.id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    label_div.child(dropdown)
}
