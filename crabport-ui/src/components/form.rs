//! Shared builders for the modal form dialogs (connection / tunnel /
//! snippet / group): labeled fields, footers, and the dialog shell.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::{AnimatedWrapper, TransitionExt};
use gpui_component::input::InputState;

use crabport_core::credential::{GroupEntry, GroupKind};

use crate::app::CrabportApp;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::input::{StyledInput, StyledPasswordInput};
use crate::motion::{EASE_STANDARD, RADIUS_LG, duration_base};

// ---------------------------------------------------------------------------
// Dialog shell
// ---------------------------------------------------------------------------

/// Dialog chrome shared by the connection / tunnel / snippet / group form
/// dialogs: fixed width, base background + border + `RADIUS_LG` corners +
/// shadow, column flex with `gap_4`, a click-swallower while open (so clicks
/// inside the dialog don't reach the backdrop's close handler), and the
/// open/close fade + slide transition keyed on `id`.
///
/// `style` customizes the dialog div before the transition wrapper is
/// attached (e.g. `p_6()` padding, or the connection form's `max_h` +
/// `overflow_hidden`). Interactive handlers would not survive the
/// `AnimatedWrapper`, so only style tweaks belong there; callers chain
/// `.child(...)` onto the returned wrapper.
pub(crate) fn form_dialog(
    id: &'static str,
    open: bool,
    width: f32,
    style: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
) -> AnimatedWrapper<Stateful<Div>> {
    let dialog_id = ElementId::Name(id.into());
    div()
        .id(dialog_id.clone())
        .w(px(width))
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded(RADIUS_LG)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_4()
        .map(style)
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
}

/// Dialog title row (large semibold primary text).
pub(crate) fn form_title(title: String) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(text_primary()))
        .child(title)
}

// ---------------------------------------------------------------------------
// Footer (cancel + confirm row)
// ---------------------------------------------------------------------------

/// Cancel + confirm footer row shared by the form dialogs. The cancel button
/// fires `on_cancel`; the confirm button is primary-styled and runs
/// `on_confirm` (each form supplies its own transition-reset / validate /
/// save logic there).
pub(crate) fn form_footer(
    cancel_id: &'static str,
    cancel_label: String,
    on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    confirm_id: &'static str,
    confirm_label: String,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new(cancel_id)
                .centered(true)
                .child(cancel_label)
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_cancel {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new(confirm_id)
                .primary()
                .centered(true)
                .child(confirm_label)
                .on_click(on_confirm),
        )
}

// ---------------------------------------------------------------------------
// Labeled fields
// ---------------------------------------------------------------------------

/// Labeled single-line text input with an optional validation error. Callers
/// may chain further `StyledInput` builders (`.multi_line(true)`, `.rows(n)`,
/// `.prefix(..)`, …).
pub(crate) fn text_field(
    id: &'static str,
    state: Entity<InputState>,
    label: String,
    error: Option<SharedString>,
) -> StyledInput {
    StyledInput::new(id, state)
        .label(label)
        .when_some(error, |el, e| el.error(e))
}

/// Labeled password input (masked shell) with an optional validation error.
pub(crate) fn password_field(
    id: &'static str,
    state: Entity<InputState>,
    label: String,
    error: Option<SharedString>,
) -> StyledPasswordInput {
    StyledPasswordInput::new(id, state)
        .label(label)
        .when_some(error, |el, e| el.error(e))
}

/// Field label wrapper used above the connection form's dropdown controls: a
/// flex column so the control sits `gap_1` (4px) below the label text.
pub(crate) fn label_column(label: String) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(label)
}

/// Field label wrapper used above the tunnel / snippet forms' dropdown
/// controls: block layout (no flex column, no gap). Kept distinct from
/// [`label_column`] because the two families render differently today.
pub(crate) fn label_block(label: String) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(label)
}

// ---------------------------------------------------------------------------
// Group dropdown
// ---------------------------------------------------------------------------

/// Searchable + creatable group dropdown shared by the connection / tunnel /
/// snippet forms. Items are `[none_label] ++ groups`; index 0 is the "None"
/// (ungrouped) sentinel whose value is `none_value` (`"none"` for the
/// connection and tunnel forms, `""` for the snippet form — preserved
/// as-is). `fields` resolves the owning form's
/// `(group_id, group_dropdown_open)` pair on `CrabportApp` so the shared
/// callbacks write back to the right `*FormState`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn group_dropdown(
    id: &'static str,
    placeholder: String,
    none_label: String,
    none_value: &'static str,
    kind: GroupKind,
    group_id: Option<i64>,
    open: bool,
    search_input: Entity<InputState>,
    groups: Vec<GroupEntry>,
    app: Entity<CrabportApp>,
    fields: fn(&mut CrabportApp) -> Option<(&mut Option<i64>, &mut bool)>,
) -> Dropdown {
    // Index 0 = "None" (ungrouped); groups start at index 1.
    let selected_idx =
        group_id.and_then(|id| groups.iter().position(|g| g.id == id).map(|i| i + 1));

    let mut dropdown = Dropdown::new(id)
        .placeholder(placeholder)
        .is_open(open)
        .searchable(search_input)
        .on_create({
            let app = app.clone();
            move |name, _w, cx| {
                app.update(cx, |app, cx| {
                    // Create the group, then immediately select it.
                    if let Ok(gid) = AppState::store(cx).lock().add_group(&name, kind, None) {
                        if let Some((group_id, dropdown_open)) = fields(app) {
                            *group_id = Some(gid);
                            *dropdown_open = false;
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
                    if let Some((_, dropdown_open)) = fields(app) {
                        *dropdown_open = !*dropdown_open;
                        cx.notify();
                    }
                });
            }
        })
        .on_change({
            let groups = groups.clone();
            move |index, _w, cx| {
                let new_group = if index == 0 {
                    None
                } else {
                    groups.get(index - 1).map(|g| g.id)
                };
                app.update(cx, |app, cx| {
                    if let Some((group_id, dropdown_open)) = fields(app) {
                        *group_id = new_group;
                        *dropdown_open = false;
                        cx.notify();
                    }
                });
            }
        });

    dropdown = dropdown.item_with_value(none_label, none_value);
    for g in &groups {
        dropdown = dropdown.item_with_value(g.name.clone(), g.id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }
    dropdown
}
