use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::time::Duration;

use crate::color::*;

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
// Command — shadcn/ui Command-like palette
// ---------------------------------------------------------------------------

/// A shadcn/ui `Command`-style dialog palette for creating new connections.
///
/// # Usage
///
/// ```ignore
/// let search_state = cx.new(|cx| InputState::new(window, cx));
/// Command::new()
///     .open(show_command)
///     .search_state(search_state)
///     .hosts(my_hosts)
///     .on_close(|w, cx| { ... })
///     .on_select_host(|i, w, cx| { ... })
///     .on_new_connection(|ct, w, cx| { ... })
/// ```
#[derive(IntoElement)]
pub struct Command {
    /// Whether the dialog is currently open (triggers entry animation).
    open: bool,
    /// gpui-component `InputState` entity for the search field.
    /// When `None` a non-interactive placeholder is rendered instead.
    search_state: Option<Entity<gpui_component::input::InputState>>,
    /// List of saved host names shown under the *Hosts* group.
    hosts: Vec<String>,
    /// Called when the user clicks the overlay background to dismiss.
    on_close: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// Called when a host item is clicked (index into `hosts`).
    on_select_host: Option<std::rc::Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
    /// Called when one of the *New Connection* items is clicked.
    on_new_connection: Option<std::rc::Rc<dyn Fn(ConnectionType, &mut Window, &mut App) + 'static>>,
}

impl Command {
    pub fn new() -> Self {
        Self {
            open: false,
            search_state: None,
            hosts: Vec::new(),
            on_close: None,
            on_select_host: None,
            on_new_connection: None,
        }
    }

    /// Show / hide the dialog (controls the entry / exit transition).
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Attach a gpui-component `InputState` entity so the search bar
    /// becomes an interactive text field.
    pub fn search_state(mut self, state: Entity<gpui_component::input::InputState>) -> Self {
        self.search_state = Some(state);
        self
    }

    /// Saved host names to display in the *Hosts* group.
    /// When the list is empty the group is hidden.
    pub fn hosts(mut self, hosts: Vec<String>) -> Self {
        self.hosts = hosts;
        self
    }

    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(std::rc::Rc::new(f));
        self
    }

    pub fn on_select_host(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select_host = Some(std::rc::Rc::new(f));
        self
    }

    pub fn on_new_connection(
        mut self,
        f: impl Fn(ConnectionType, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_new_connection = Some(std::rc::Rc::new(f));
        self
    }
}

// ---------------------------------------------------------------------------
// RenderOnce
// ---------------------------------------------------------------------------

impl RenderOnce for Command {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let has_hosts = !self.hosts.is_empty();
        let dialog_id = ElementId::Name("command-dialog".into());

        // -- Search bar ---------------------------------------------------
        let search: AnyElement = if let Some(state) = self.search_state {
            gpui_component::input::Input::new(&state)
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
        };

        // -- Overlay + Dialog ---------------------------------------------
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
            // Block mouse when open, pass through when closed
            .when(self.open, |el| {
                el.occlude().on_mouse_down(MouseButton::Left, {
                    let on_close = self.on_close.clone();
                    move |_e, w, cx| {
                        if let Some(ref cb) = on_close {
                            cb(w, cx);
                        }
                    }
                })
            })
            // Overlay background fades in/out with the open state
            .with_transition(overlay_id)
            .transition_when_else(
                self.open,
                Duration::from_millis(150),
                Linear,
                |el| el.bg(rgba(COMMAND_OVERLAY)),
                |el| el.bg(rgba(0x00000000)),
            )
            // Dialog panel
            .child({
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
                    // Base style matches closed state (prevents first-frame flash)
                    .opacity(0.0)
                    .mt(px(-16.0))
                    // Stop clicks on the panel from bubbling to the overlay
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // --- Open / close transition (slide down from above) ---
                    .with_transition(dialog_id)
                    .transition_when_else(
                        self.open,
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
                            .overflow_y_scrollbar()
                            .p_2()
                            .flex()
                            .flex_col()
                            // ---- Hosts group ----
                            .when(has_hosts, |el| {
                                el.child(group_label(t!("new_connection.hosts")))
                                    .children(self.hosts.iter().enumerate().map(|(i, host)| {
                                        let host = host.clone();
                                        let on_select = self.on_select_host.clone();
                                        command_item(
                                            ElementId::Name(format!("cmd-host-{i}").into()),
                                            "icons/server.svg",
                                            host.clone(),
                                            None::<SharedString>,
                                            self.open,
                                            move |w, cx| {
                                                if let Some(ref cb) = on_select {
                                                    cb(i, w, cx);
                                                }
                                            },
                                        )
                                    }))
                                    .child(div().h_px().bg(rgb(COMMAND_BORDER)).mx_1().my_1())
                            })
                            // ---- New Connection group ----
                            .child(group_label(t!("new_connection.title")))
                            .children(ConnectionType::all().into_iter().map(|ct| {
                                let on_new = self.on_new_connection.clone();
                                command_item(
                                    ElementId::Name(format!("cmd-conn-{ct:?}").into()),
                                    ct.icon(),
                                    ct.label(),
                                    Some(ct.description()),
                                    self.open,
                                    move |w, cx| {
                                        if let Some(ref cb) = on_new {
                                            cb(ct, w, cx);
                                        }
                                    },
                                )
                            })),
                    )
            })
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A single command-palette item row (icon + label + optional description).
fn command_item(
    id: ElementId,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    description: Option<impl Into<SharedString>>,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let desc = description.map(|d| d.into());

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
            el.cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_e, w, cx| on_click(w, cx))
        })
        .with_transition(id)
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
                        .text_sm()
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(label.clone()),
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

/// Section heading inside the scrollable list (e.g. "Hosts" / "New Connection").
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
