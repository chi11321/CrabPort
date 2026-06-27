use gpui::{prelude::FluentBuilder, *};
use gpui_animation::animation::AnimatedWrapper;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::rc::Rc;
use std::time::Duration;

use crate::color::*;
use crate::views::hosts::ConnectionHost;

// ---------------------------------------------------------------------------
// Connection type
// ---------------------------------------------------------------------------

/// Types of new connections the user can create from the command palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionType {
    LocalTerminal,
    SSH,
    SFTP,
    Telnet,
    Serial,
}

impl ConnectionType {
    pub fn label(&self) -> SharedString {
        match self {
            ConnectionType::LocalTerminal => t!("new_connection.local_terminal").into(),
            ConnectionType::SSH => t!("new_connection.ssh").into(),
            ConnectionType::SFTP => t!("new_connection.sftp").into(),
            ConnectionType::Telnet => t!("new_connection.telnet").into(),
            ConnectionType::Serial => t!("new_connection.serial").into(),
        }
    }

    pub fn description(&self) -> SharedString {
        match self {
            ConnectionType::LocalTerminal => t!("new_connection.local_terminal_desc").into(),
            ConnectionType::SSH => t!("new_connection.ssh_desc").into(),
            ConnectionType::SFTP => t!("new_connection.sftp_desc").into(),
            ConnectionType::Telnet => t!("new_connection.telnet_desc").into(),
            ConnectionType::Serial => t!("new_connection.serial_desc").into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        "icons/square-terminal.svg"
    }

    pub fn all() -> [ConnectionType; 5] {
        [
            ConnectionType::LocalTerminal,
            ConnectionType::SSH,
            ConnectionType::SFTP,
            ConnectionType::Telnet,
            ConnectionType::Serial,
        ]
    }
}

// ---------------------------------------------------------------------------
// CommandView
// ---------------------------------------------------------------------------

pub struct CommandView {
    pub open: bool,
    search_state: Option<Entity<InputState>>,
    hosts: Vec<ConnectionHost>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_select_host: Option<Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
    on_new_connection: Option<Rc<dyn Fn(ConnectionType, &mut Window, &mut App) + 'static>>,
}

impl CommandView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            open: false,
            search_state: None,
            hosts: Vec::new(),
            on_close: None,
            on_select_host: None,
            on_new_connection: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        // Lazy-init search InputState
        if self.search_state.is_none() {
            self.search_state = Some(cx.new(|cx| InputState::new(window, cx)));
        }
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    pub fn set_hosts(&mut self, hosts: Vec<ConnectionHost>) {
        self.hosts = hosts;
    }

    pub fn set_on_close(&mut self, f: impl Fn(&mut Window, &mut App) + 'static) {
        self.on_close = Some(Rc::new(f));
    }

    pub fn set_on_select_host(&mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) {
        self.on_select_host = Some(Rc::new(f));
    }

    pub fn set_on_new_connection(
        &mut self,
        f: impl Fn(ConnectionType, &mut Window, &mut App) + 'static,
    ) {
        self.on_new_connection = Some(Rc::new(f));
    }
}

impl Render for CommandView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let is_open = self.open;
        let search = render_search_bar(self.search_state.as_ref());
        let on_close = self.on_close.clone();
        let on_select_host = self.on_select_host.clone();
        let on_new_connection = self.on_new_connection.clone();
        let hosts = self.hosts.clone();

        render_overlay(is_open, on_close).child(render_dialog(
            is_open,
            search,
            hosts,
            on_select_host,
            on_new_connection,
        ))
    }
}

// ---------------------------------------------------------------------------
// Extracted render helpers
// ---------------------------------------------------------------------------

fn render_search_bar(search_state: Option<&Entity<InputState>>) -> AnyElement {
    if let Some(state) = search_state {
        gpui_component::input::Input::new(state)
            .prefix(
                svg()
                    .path("icons/search.svg")
                    .size_4()
                    .text_color(rgb(TEXT_MUTED)),
            )
            .appearance(false)
            .bordered(false)
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap_2()
            .h_8()
            .child(
                svg()
                    .path("icons/search.svg")
                    .size_4()
                    .text_color(rgb(TEXT_MUTED)),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(TEXT_MUTED))
                    .child(t!("new_connection.search").to_string()),
            )
            .into_any_element()
    }
}

fn render_overlay(
    is_open: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
) -> AnimatedWrapper<Stateful<gpui::Div>> {
    let overlay_id = ElementId::Name("command-overlay".into());

    div()
        .id(overlay_id.clone())
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .flex()
        .items_start()
        .justify_center()
        .pt_16()
        .bg(rgba(0x00000000))
        .when(is_open, |el| {
            el.occlude().on_click(move |_e, w, cx| {
                if let Some(ref cb) = on_close {
                    cb(w, cx);
                }
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            is_open,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(COMMAND_OVERLAY)),
            |el| el.bg(rgba(0x00000000)),
        )
}

fn render_dialog(
    is_open: bool,
    search: AnyElement,
    hosts: Vec<ConnectionHost>,
    on_select_host: Option<Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
    on_new_connection: Option<Rc<dyn Fn(ConnectionType, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("command-dialog".into());

    div()
        .id(dialog_id.clone())
        .w(px(520.0))
        .max_h(px(420.0))
        .bg(rgb(COMMAND_BG))
        .border_1()
        .border_color(rgb(COMMAND_BORDER))
        .rounded_lg()
        .shadow_lg()
        .flex()
        .flex_col()
        .overflow_hidden()
        .opacity(0.0)
        .mt(px(-16.0))
        .when(is_open, |el| {
            el.on_click(|_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            is_open,
            Duration::from_millis(150),
            Linear,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // --- Search bar ---
        .child(
            div()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(rgb(COMMAND_BORDER))
                .child(search),
        )
        // --- Scrollable item list ---
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .p_2()
                .flex()
                .flex_col()
                .child(render_hosts_list(hosts, is_open, on_select_host))
                .child(render_connection_list(is_open, on_new_connection)),
        )
}

fn render_hosts_list(
    hosts: Vec<ConnectionHost>,
    is_open: bool,
    on_select_host: Option<Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    div().when(!hosts.is_empty(), |el| {
        el.child(group_label(t!("new_connection.hosts")))
            .children(hosts.into_iter().take(5).enumerate().map(|(i, host)| {
                let on_select = on_select_host.clone();
                let is_favorite = host.favorite;
                command_item(
                    ElementId::Name(format!("cmd-host-{i}").into()),
                    "icons/server.svg",
                    host.name.clone(),
                    Some(format!("{}@{}:{}", host.username, host.host, host.port)),
                    is_open,
                    is_favorite,
                    move |w, cx| {
                        if let Some(ref cb) = on_select {
                            cb(i, w, cx);
                        }
                    },
                )
            }))
            .child(div().h_px().bg(rgb(COMMAND_BORDER)).mx_1().my_1())
    })
}

fn render_connection_list(
    is_open: bool,
    on_new_connection: Option<Rc<dyn Fn(ConnectionType, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    div()
        .child(group_label(t!("new_connection.title")))
        .children(ConnectionType::all().into_iter().map(|ct| {
            let on_new = on_new_connection.clone();
            command_item(
                ElementId::Name(format!("cmd-conn-{ct:?}").into()),
                ct.icon(),
                ct.label(),
                Some(ct.description()),
                is_open,
                false,
                move |w, cx| {
                    if let Some(ref cb) = on_new {
                        cb(ct, w, cx);
                    }
                },
            )
        }))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn command_item(
    id: ElementId,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    description: Option<impl Into<SharedString>>,
    enabled: bool,
    is_favorite: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let desc = description.map(|d| d.into());

    let id_for_reset = id.clone();
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_2()
        .rounded_sm()
        .bg(rgb(COMMAND_BG))
        .when(enabled, |el| {
            el.cursor_pointer().on_click(move |_e, w, cx| {
                gpui_animation::reset_transition(&id_for_reset);
                on_click(w, cx);
            })
        })
        .with_transition(id.clone())
        .transition_on_hover(Duration::from_millis(120), Linear, |hovered, el| {
            if *hovered {
                el.bg(rgb(COMMAND_ITEM_HOVER))
            } else {
                el.bg(rgb(COMMAND_BG))
            }
        })
        .child(
            svg()
                .path(icon_path)
                .size_4()
                .text_color(rgb(TEXT_MUTED))
                .flex_shrink_0(),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(TEXT_PRIMARY))
                                .child(label.clone()),
                        )
                        .when(is_favorite, |el| {
                            el.child(
                                svg()
                                    .path("icons/star.svg")
                                    .size_3()
                                    .text_color(rgb(0xf9e2af)), // TERM_YELLOW
                            )
                        }),
                )
                .when_some(desc, |el, desc| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .mt_0p5()
                            .child(desc),
                    )
                }),
        )
}

fn group_label(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(COMMAND_GROUP_LABEL))
        .child(text.into())
}
