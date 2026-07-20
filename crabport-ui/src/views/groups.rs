//! Shared group form dialog (create / rename) for all collection kinds.
//!
//! A small overlay dialog used by the Hosts, Snippets, and Tunnels views to
//! create a new group or rename an existing one. The dialog is parameterized
//! by [`GroupKind`] so a single state type + renderer serves every domain.
//!
//! `GroupFormState` is owned by `CrabportApp` (`app.group_form`) and is `None`
//! when closed. `GroupFormView` is a pure `RenderOnce` renderer that snapshots
//! the state. Persistence is delegated to `CrabportApp::save_group_form`,
//! which dispatches to the matching store CRUD method based on `kind`.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind};

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::input::StyledInput;
use crate::components::overlay::render_overlay;
use crate::motion::{EASE_STANDARD, RADIUS_LG, duration_base};

// ---------------------------------------------------------------------------
// Output passed to the save callback
// ---------------------------------------------------------------------------

/// Parsed form values handed to the save callback. The caller resolves
/// `editing_id` against the store (UPDATE if `Some`, INSERT if `None`).
#[derive(Clone, Debug)]
pub struct GroupFormOutput {
    pub editing_id: Option<i64>,
    pub kind: GroupKind,
    pub name: String,
}

// ---------------------------------------------------------------------------
// GroupFormState — owned by CrabportApp
// ---------------------------------------------------------------------------

/// Holds all mutable state for the group form overlay so that
/// [`GroupFormView`] can be a pure `RenderOnce` renderer.
#[derive(Clone)]
pub struct GroupFormState {
    /// `Some(id)` when renaming an existing group; `None` when creating.
    pub editing_id: Option<i64>,
    /// Which collection this group belongs to. Set on open so the save
    /// callback dispatches to the right store CRUD path.
    pub kind: GroupKind,
    pub name_input: Entity<InputState>,
    pub name_focused: bool,
    /// Open/close animation state. `true` while the overlay is visible.
    pub open: bool,
    /// Per-field validation error. Populated by `validate()`.
    pub name_error: Option<SharedString>,
    pub on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub on_save: Option<Rc<dyn Fn(GroupFormOutput, &mut Window, &mut App) + 'static>>,
}

impl GroupFormState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        Self {
            editing_id: None,
            kind: GroupKind::Host,
            name_input,
            name_focused: false,
            open: false,
            name_error: None,
            on_close: None,
            on_save: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.open = true;
        self.name_error = None;
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Reset all fields to blank defaults and open the dialog in create mode.
    pub fn open_for_create(&mut self, kind: GroupKind, window: &mut Window, cx: &mut App) {
        self.editing_id = None;
        self.kind = kind;
        self.name_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.open(window, cx);
    }

    /// Populate the fields from an existing group, set `editing_id`, and open
    /// the dialog in rename mode.
    pub fn open_for_edit(&mut self, group: &GroupEntry, window: &mut Window, cx: &mut App) {
        let name = group.name.clone();
        self.editing_id = Some(group.id);
        self.kind = group.kind;
        self.name_input
            .update(cx, |state, cx| state.set_value(&name, window, cx));
        self.open(window, cx);
    }

    pub fn name_text(&self, cx: &App) -> String {
        self.name_input.read(cx).text().to_string()
    }

    /// Validate the name field. Populates `self.name_error` and returns `true`
    /// if valid.
    pub fn validate(&mut self, cx: &App) -> bool {
        if self.name_text(cx).trim().is_empty() {
            self.name_error = Some(t!("groups.error_name_required").into());
            false
        } else {
            self.name_error = None;
            true
        }
    }

    /// Build the [`GroupFormOutput`] from the current state. Does NOT validate
    /// — call `validate()` first.
    pub fn output(&self, cx: &App) -> GroupFormOutput {
        GroupFormOutput {
            editing_id: self.editing_id,
            kind: self.kind,
            name: self.name_text(cx),
        }
    }
}

// ---------------------------------------------------------------------------
// GroupFormView — pure RenderOnce renderer
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct GroupFormView {
    open: bool,
    editing: bool,
    name_input: Entity<InputState>,
    name_focused: bool,
    name_error: Option<SharedString>,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(GroupFormOutput, &mut Window, &mut App) + 'static>>,
}

impl GroupFormView {
    pub fn new(state: &GroupFormState, app: Entity<CrabportApp>) -> Self {
        Self {
            open: state.open,
            editing: state.editing_id.is_some(),
            name_input: state.name_input.clone(),
            name_focused: state.name_focused,
            name_error: state.name_error.clone(),
            app,
            on_close: state.on_close.clone(),
            on_save: state.on_save.clone(),
        }
    }
}

impl RenderOnce for GroupFormView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();

        render_overlay(
            ElementId::Name("group-form-overlay".into()),
            self.open,
            self.on_close,
            render_dialog(
                self.open,
                self.editing,
                self.name_input,
                self.name_focused,
                self.name_error,
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
    name_focused: bool,
    name_error: Option<SharedString>,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(GroupFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("group-form-dialog".into());

    let title = if editing {
        t!("groups.rename_title").to_string()
    } else {
        t!("groups.new_title").to_string()
    };

    div()
        .id(dialog_id.clone())
        .w(px(380.0))
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
            duration_base(),
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
            div().child(
                StyledInput::new("group-form-name", name_input)
                    .label(t!("groups.name").to_string())
                    .focused(name_focused)
                    .when_some(name_error, |el, e| el.error(e)),
            ),
        )
        // Buttons
        .child(render_buttons(app, on_close, on_save))
}

fn render_buttons(
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(GroupFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("group-form-overlay".into());
    let dialog_id = ElementId::Name("group-form-dialog".into());

    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new("group-form-cancel")
                .centered(true)
                .child(t!("groups.cancel").to_string())
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new("group-form-save")
                .primary()
                .centered(true)
                .child(t!("groups.save").to_string())
                .on_click(move |_e, w, cx| {
                    // Reset the overlay/dialog transitions so the next open
                    // starts fresh (mirrors snippet form's save button).
                    gpui_animation::reset_transition(&overlay_id);
                    gpui_animation::reset_transition(&dialog_id);
                    // Validate required fields before building the output.
                    let output: Option<GroupFormOutput> = app.update(cx, |app, cx| {
                        let valid = app
                            .group_form
                            .as_mut()
                            .map(|form| form.validate(cx))
                            .unwrap_or(true);
                        if !valid {
                            cx.notify();
                            return None;
                        }
                        app.group_form.as_ref().map(|form| form.output(cx))
                    });
                    if let Some(out) = output {
                        if let Some(ref cb) = on_save {
                            cb(out, w, cx);
                        }
                    }
                }),
        )
}
