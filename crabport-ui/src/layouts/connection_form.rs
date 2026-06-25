use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::{Input, InputState};
use rust_i18n::t;
use std::time::Duration;

use crate::color::*;

// ---------------------------------------------------------------------------
// Connection type
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionKind {
    SSH,
    Telnet,
    Serial,
}

// ---------------------------------------------------------------------------
// Form state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ConnectionFormState {
    pub active: bool,
    pub kind: ConnectionKind,
}

impl ConnectionFormState {
    pub fn new() -> Self {
        Self {
            active: false,
            kind: ConnectionKind::SSH,
        }
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

pub fn render_connection_form(
    state: &ConnectionFormState,
    host_input: &Option<Entity<InputState>>,
    port_input: &Option<Entity<InputState>>,
    user_input: &Option<Entity<InputState>>,
    pass_input: &Option<Entity<InputState>>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
    on_connect: impl Fn(ConnectionKind, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let active = state.active;
    let kind = state.kind;
    let on_close = std::rc::Rc::new(on_close);
    let on_connect = std::rc::Rc::new(on_connect);

    let overlay_id = ElementId::Name("conn-form-overlay".into());
    let dialog_id = ElementId::Name("conn-form-dialog".into());

    div()
        .id(overlay_id.clone())
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .flex()
        .items_center()
        .justify_center()
        // Block mouse when active, pass through when inactive
        .when(active, |el| {
            el.occlude().on_mouse_down(MouseButton::Left, {
                let on_close = on_close.clone();
                move |_e, w, cx| on_close(w, cx)
            })
        })
        // Overlay background fades in/out
        .with_transition(overlay_id)
        .transition_when_else(
            active,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(0x00000080)),
            |el| el.bg(rgba(0x00000000)),
        )
        // Dialog panel
        .child(
            div()
                .id(dialog_id.clone())
                .w(px(420.0))
                .bg(rgb(BG_BASE))
                .border_1()
                .border_color(rgb(BORDER))
                .rounded_lg()
                .shadow_lg()
                .flex()
                .flex_col()
                .p_6()
                .gap_4()
                // Closed-state: transparent, above viewport
                .opacity(0.0)
                .mt(px(-16.0))
                // Stop clicks from bubbling to overlay
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                // Open / close transition (slide down + fade)
                .with_transition(dialog_id)
                .transition_when_else(
                    active,
                    Duration::from_millis(150),
                    Linear,
                    |el| el.opacity(1.0).mt_0(),
                    |el| el.opacity(0.0).mt(px(-16.0)),
                )
                // --- Title ---
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(t!("connection_form.title").to_string()),
                )
                // --- Type selector ---
                .child({
                    let k = kind;
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .bg(rgb(SURFACE_ACTIVE))
                        .rounded_md()
                        .p_0p5()
                        .child(type_tab(ConnectionKind::SSH, k, t!("new_connection.ssh")))
                        .child(type_tab(
                            ConnectionKind::Telnet,
                            k,
                            t!("new_connection.telnet"),
                        ))
                        .child(type_tab(
                            ConnectionKind::Serial,
                            k,
                            t!("new_connection.serial"),
                        ))
                })
                // --- Host ---
                .child(form_input(&t!("connection_form.host"), host_input))
                // --- Port ---
                .child(form_input(&t!("connection_form.port"), port_input))
                // --- Username ---
                .child(form_input(&t!("connection_form.username"), user_input))
                // --- Password ---
                .child(form_input(&t!("connection_form.password"), pass_input))
                // --- Buttons ---
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .justify_end()
                        .child(
                            div()
                                .px_4()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(BTN_BG))
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_sm()
                                .child(t!("connection_form.cancel").to_string())
                                .on_mouse_down(MouseButton::Left, {
                                    let on_close = on_close.clone();
                                    move |_e, w, cx| {
                                        on_close(w, cx);
                                    }
                                }),
                        )
                        .child(
                            div()
                                .px_4()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(0x3b82f6))
                                .text_color(rgb(0xffffff))
                                .text_sm()
                                .child(t!("connection_form.connect").to_string())
                                .on_mouse_down(MouseButton::Left, {
                                    let on_connect = on_connect.clone();
                                    move |_e, w, cx| {
                                        on_connect(kind, w, cx);
                                    }
                                }),
                        ),
                ),
        )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn type_tab(
    kind: ConnectionKind,
    active: ConnectionKind,
    label: impl Into<SharedString>,
) -> impl IntoElement {
    let is_active = kind == active;
    let label = label.into();
    div()
        .flex_1()
        .px_3()
        .py_1()
        .rounded_sm()
        .text_sm()
        .text_center()
        .when(is_active, |el| {
            el.bg(rgb(BG_BASE)).text_color(rgb(TEXT_PRIMARY))
        })
        .when(!is_active, |el| el.text_color(rgb(TEXT_MUTED)))
        .child(label.clone())
}

fn form_input(label: &str, input: &Option<Entity<InputState>>) -> impl IntoElement {
    let input_id = ElementId::Name(format!("input-{}", label).into());
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(TEXT_MUTED))
                .child(label.to_string()),
        )
        .child({
            div()
                .id(input_id.clone())
                .border_1()
                .border_color(rgb(BORDER))
                .rounded_md()
                .overflow_hidden()
                .h_8()
                .when_some(input.as_ref(), |el, state| el.child(Input::new(state)))
        })
}
