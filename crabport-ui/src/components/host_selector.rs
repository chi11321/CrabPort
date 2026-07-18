//! Host selector overlay for the SFTP tab's local and remote panels.
//!
//! Renders a command-palette-style overlay listing "Local" at the top
//! followed by all saved SSH hosts (favorites first, then by
//! `last_login` descending). Clicking an item invokes `on_select` with
//! `None` (Local) or `Some(host_id)`, then closes.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::AnimatedWrapper;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::rc::Rc;

use crate::color::*;
use crate::motion::{DURATION_BASE, DURATION_FAST, EASE_LINEAR, RADIUS_LG, RADIUS_SM};
use crate::views::sessions::ConnectionHost;
use crate::views::sessions::ConnectionKind;

/// Which panel opened the host selector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
pub enum PanelSide {
    Left = 0,
    Right = 1,
}

/// Overlay shown when the user clicks the host label button at the top
/// of either SFTP panel. Mirrors the command palette's visual style
/// but only lists "Local" + saved SSH hosts.
pub struct HostSelectorOverlay {
    pub open: bool,
    search_state: Option<Entity<InputState>>,
    /// Current search query, kept in sync via an `InputEvent::Change`
    /// subscription on `search_state`.
    search_query: String,
    hosts: Vec<ConnectionHost>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// `None` = Local, `Some(host_id)` = remote host.
    on_select: Option<Rc<dyn Fn(Option<i64>, &mut Window, &mut App) + 'static>>,
}

impl HostSelectorOverlay {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            open: false,
            search_state: None,
            search_query: String::new(),
            hosts: Vec::new(),
            on_close: None,
            on_select: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        // Lazy-init search InputState + subscribe to changes for live
        // filtering.
        if self.search_state.is_none() {
            let entity = cx.new(|cx| InputState::new(window, cx));
            cx.subscribe(
                &entity,
                |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::Change { .. } = event {
                        this.search_query = input.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            )
            .detach();
            self.search_state = Some(entity);
        }
        // Clear any previous query when re-opening.
        self.search_query.clear();
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

    pub fn set_on_select(&mut self, f: impl Fn(Option<i64>, &mut Window, &mut App) + 'static) {
        self.on_select = Some(Rc::new(f));
    }
}

impl Render for HostSelectorOverlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let is_open = self.open;
        let search = render_search_bar(self.search_state.as_ref());
        let on_close = self.on_close.clone();
        let on_select = self.on_select.clone();
        let hosts = self.hosts.clone();
        let query = self.search_query.clone();

        render_overlay(is_open, on_close)
            .child(render_dialog(is_open, search, hosts, query, on_select))
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
                    .text_color(rgb(text_muted())),
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
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .child(t!("sftp_tab.select_host").to_string()),
            )
            .into_any_element()
    }
}

fn render_overlay(
    is_open: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
) -> AnimatedWrapper<Stateful<gpui::Div>> {
    let overlay_id = ElementId::Name("host-selector-overlay".into());

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
            DURATION_BASE,
            EASE_LINEAR,
            |el| el.bg(rgba(command_overlay())),
            |el| el.bg(rgba(0x00000000)),
        )
}

fn render_dialog(
    is_open: bool,
    search: AnyElement,
    hosts: Vec<ConnectionHost>,
    query: String,
    on_select: Option<Rc<dyn Fn(Option<i64>, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("host-selector-dialog".into());

    div()
        .id(dialog_id.clone())
        .w(px(520.0))
        .max_h(px(420.0))
        .bg(rgb(command_bg()))
        .border_1()
        .border_color(rgb(command_border()))
        .rounded(RADIUS_LG)
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
            DURATION_BASE,
            EASE_LINEAR,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // --- Search bar ---
        .child(
            div()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(rgb(command_border()))
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
                .child(render_items_list(hosts, query, is_open, on_select)),
        )
}

fn render_items_list(
    hosts: Vec<ConnectionHost>,
    query: String,
    is_open: bool,
    on_select: Option<Rc<dyn Fn(Option<i64>, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    // Live-filter hosts by the current search query. Case-insensitive
    // match on name / host / username. Empty query shows all.
    // Only SSH hosts are shown — Telnet and Serial don't support SFTP.
    let q = query.trim().to_lowercase();
    let filtered: Vec<ConnectionHost> = hosts
        .into_iter()
        .filter(|h| h.kind == ConnectionKind::SSH)
        .filter(|h| {
            if q.is_empty() {
                true
            } else {
                h.name.to_lowercase().contains(&q)
                    || h.host.to_lowercase().contains(&q)
                    || h.username.to_lowercase().contains(&q)
            }
        })
        .collect();

    let local_on_select = on_select.clone();
    let local_id = ElementId::Name("host-selector-local".into());

    div()
        // --- Local entry (always at top) ---
        .child(group_label(t!("sftp_tab.select_host")))
        .child(host_item(
            local_id,
            "icons/folder.svg",
            t!("sftp_tab.local_entry").to_string(),
            Some(t!("sftp_tab.local_entry_desc").to_string()),
            is_open,
            false,
            move |w, cx| {
                if let Some(ref cb) = local_on_select {
                    cb(None, w, cx);
                }
            },
        ))
        .child(div().h_px().bg(rgb(command_border())).mx_1().my_1())
        // --- Saved hosts ---
        .when_else(
            !filtered.is_empty(),
            |el| {
                el.child(group_label(t!("sidebar.sessions"))).children(
                    filtered.into_iter().enumerate().map(|(i, host)| {
                        let on_select = on_select.clone();
                        let host_id = host.id;
                        let is_favorite = host.favorite;
                        host_item(
                            ElementId::Name(format!("host-selector-host-{i}").into()),
                            "icons/server.svg",
                            host.name.clone(),
                            Some(format!("{}@{}:{}", host.username, host.host, host.port)),
                            is_open,
                            is_favorite,
                            move |w, cx| {
                                if let Some(ref cb) = on_select {
                                    cb(Some(host_id), w, cx);
                                }
                            },
                        )
                    }),
                )
            },
            |el| el,
        )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn host_item(
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
        .rounded(RADIUS_SM)
        .bg(rgb(command_bg()))
        .when(enabled, |el| {
            el.on_click(move |_e, w, cx| {
                gpui_animation::reset_transition(&id_for_reset);
                on_click(w, cx);
            })
        })
        .with_transition(id.clone())
        .transition_on_hover(DURATION_FAST, EASE_LINEAR, |hovered, el| {
            if *hovered {
                el.bg(rgb(command_item_hover()))
            } else {
                el.bg(rgb(command_bg()))
            }
        })
        .child(
            svg()
                .path(icon_path)
                .size_4()
                .text_color(rgb(text_muted()))
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
                                .text_color(rgb(text_primary()))
                                .child(label.clone()),
                        )
                        .when(is_favorite, |el| {
                            el.child(
                                svg()
                                    .path("icons/star.svg")
                                    .size_3()
                                    .text_color(rgb(term_yellow())),
                            )
                        }),
                )
                .when_some(desc, |el, desc| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
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
        .text_color(rgb(command_group_label()))
        .child(text.into())
}
