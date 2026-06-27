use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use rust_i18n::t;
use std::rc::Rc;
use std::time::Duration;

use crate::color::*;
use crate::components::button::Button;
use crate::components::input::{StyledInput, StyledPasswordInput};
use crate::components::segmented_control::{Segment, SegmentedControl};

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
// ConnectionFormState — owned by CrabportApp
// ---------------------------------------------------------------------------

/// Holds all mutable state for the connection form overlay so that
/// `ConnectionFormView` can be a pure `RenderOnce` renderer.
pub struct ConnectionFormState {
    pub active: bool,
    pub kind: ConnectionKind,
    pub name_input: Entity<InputState>,
    pub host_input: Entity<InputState>,
    pub port_input: Entity<InputState>,
    pub user_input: Entity<InputState>,
    pub pass_input: Entity<InputState>,
    pub name_focused: bool,
    pub host_focused: bool,
    pub port_focused: bool,
    pub user_focused: bool,
    pub pass_focused: bool,
    pub on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
}

impl ConnectionFormState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let host_input = cx.new(|cx| InputState::new(window, cx));
        let port_input = cx.new(|cx| InputState::new(window, cx));
        let user_input = cx.new(|cx| InputState::new(window, cx));
        let pass_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_masked(true, window, cx);
            state
        });

        Self {
            active: false,
            kind: ConnectionKind::SSH,
            name_input,
            host_input,
            port_input,
            user_input,
            pass_input,
            name_focused: false,
            host_focused: false,
            port_focused: false,
            user_focused: false,
            pass_focused: false,
            on_close: None,
            on_connect: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.active = true;
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        self.port_input.update(cx, |state, cx| {
            state.set_value("22", window, cx);
        });
    }

    pub fn close(&mut self) {
        self.active = false;
    }

    pub fn name_text(&self, cx: &App) -> String {
        self.name_input.read(cx).text().to_string()
    }

    pub fn host_text(&self, cx: &App) -> String {
        self.host_input.read(cx).text().to_string()
    }

    pub fn port_text(&self, cx: &App) -> String {
        self.port_input.read(cx).text().to_string()
    }

    pub fn user_text(&self, cx: &App) -> String {
        self.user_input.read(cx).text().to_string()
    }

    pub fn pass_text(&self, cx: &App) -> String {
        self.pass_input.read(cx).text().to_string()
    }
}

// ---------------------------------------------------------------------------
// ConnectionFormView — pure RenderOnce renderer
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct ConnectionFormView {
    active: bool,
    kind: ConnectionKind,
    name_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    user_input: Entity<InputState>,
    pass_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
}

impl ConnectionFormView {
    pub fn new(state: &ConnectionFormState) -> Self {
        Self {
            active: state.active,
            kind: state.kind,
            name_input: state.name_input.clone(),
            host_input: state.host_input.clone(),
            port_input: state.port_input.clone(),
            user_input: state.user_input.clone(),
            pass_input: state.pass_input.clone(),
            name_focused: state.name_focused,
            host_focused: state.host_focused,
            port_focused: state.port_focused,
            user_focused: state.user_focused,
            pass_focused: state.pass_focused,
            on_close: state.on_close.clone(),
            on_connect: state.on_connect.clone(),
        }
    }
}

impl RenderOnce for ConnectionFormView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();
        render_overlay(
            self.active,
            self.on_close,
            render_dialog(
                self.active,
                self.kind,
                self.name_input,
                self.host_input,
                self.port_input,
                self.user_input,
                self.pass_input,
                self.name_focused,
                self.host_focused,
                self.port_focused,
                self.user_focused,
                self.pass_focused,
                on_close_for_dialog,
                self.on_connect,
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_overlay(
    active: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    child: impl IntoElement,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("conn-form-overlay".into());

    div()
        .id(overlay_id.clone())
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000000))
        .when(active, |el| {
            el.occlude().on_mouse_down(MouseButton::Left, {
                move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            active,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(0x00000080)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(child)
}

fn render_dialog(
    active: bool,
    kind: ConnectionKind,
    name_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    user_input: Entity<InputState>,
    pass_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("conn-form-dialog".into());

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
        .opacity(0.0)
        .mt(px(-16.0))
        .when(active, |el| {
            el.on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            active,
            Duration::from_millis(150),
            Linear,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // Title
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_PRIMARY))
                .child(t!("connection_form.title").to_string()),
        )
        // Name
        .child(
            div().child(
                StyledInput::new("name", name_input)
                    .label(t!("connection_form.name").to_string())
                    .focused(name_focused),
            ),
        )
        // Type selector
        .child(render_type_selector(kind))
        // Host + Port row
        .child(render_host_port_row(
            host_input,
            port_input,
            host_focused,
            port_focused,
        ))
        // Username
        .child(
            div().child(
                StyledInput::new("username", user_input)
                    .label(t!("connection_form.username").to_string())
                    .focused(user_focused),
            ),
        )
        // Password
        .child(
            div().child(
                StyledPasswordInput::new("password", pass_input)
                    .label(t!("connection_form.password").to_string())
                    .focused(pass_focused)
                    .on_toggle(|_, _| {}),
            ),
        )
        // Buttons
        .child(render_buttons(kind, on_close, on_connect))
}

fn render_type_selector(kind: ConnectionKind) -> impl IntoElement {
    let active_index = match kind {
        ConnectionKind::SSH => 0,
        ConnectionKind::Telnet => 1,
        ConnectionKind::Serial => 2,
    };

    SegmentedControl::new("conn-type-selector")
        .active(active_index)
        .segment(Segment::new(t!("new_connection.ssh")))
        .segment(Segment::new(t!("new_connection.telnet")))
        .segment(Segment::new(t!("new_connection.serial")))
}

fn render_host_port_row(
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    host_focused: bool,
    port_focused: bool,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .child(
            div().flex_1().child(
                StyledInput::new("host", host_input)
                    .label(t!("connection_form.host").to_string())
                    .focused(host_focused),
            ),
        )
        .child(
            div().w(px(96.0)).child(
                StyledInput::new("port", port_input)
                    .label(t!("connection_form.port").to_string())
                    .focused(port_focused),
            ),
        )
}

fn render_buttons(
    kind: ConnectionKind,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("conn-form-overlay".into());
    let dialog_id = ElementId::Name("conn-form-dialog".into());
    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new("conn-cancel")
                .centered(true)
                .child(t!("connection_form.cancel").to_string())
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new("conn-connect")
                .primary()
                .centered(true)
                .child(t!("connection_form.connect").to_string())
                .on_click(move |_e, w, cx| {
                    gpui_animation::reset_transition(&overlay_id);
                    gpui_animation::reset_transition(&dialog_id);
                    if let Some(ref cb) = on_connect {
                        cb(kind, w, cx);
                    }
                }),
        )
}
