use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::InteractiveElementExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::rc::Rc;
use std::time::Duration;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::layouts::connection_form::{ConnectionFormState, ConnectionFormView};

/// A saved connection host entry.
#[derive(Clone)]
pub struct ConnectionHost {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub kind: crate::layouts::connection_form::ConnectionKind,
    pub credential_id: Option<i64>,
    pub last_login: Option<i64>,
    pub favorite: bool,
}

/// Render the hosts sidebar view.
///
/// Shows a list of saved hosts with a "New" button at the top that opens
/// the connection creation form.
pub fn render_hosts_view(
    hosts: &[ConnectionHost],
    form_state: Option<&ConnectionFormState>,
    app: Entity<CrabportApp>,
    on_new: impl Fn(&mut Window, &mut App) + 'static,
    on_connect: impl Fn(i64, &mut Window, &mut App) + 'static,
    on_edit: impl Fn(i64, &mut Window, &mut App) + 'static,
    on_remove: impl Fn(i64, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_connect_rc = Rc::new(on_connect);
    let on_edit_rc = Rc::new(on_edit);
    let on_remove_rc = Rc::new(on_remove);
    div()
        .size_full()
        .flex()
        .flex_col()
        .relative()
        // --- Header: title + New button ---
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_4()
                .pt_4()
                .pb_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(t!("sidebar.sessions").to_string()),
                )
                .child(
                    Button::new("hosts-new-btn")
                        .primary()
                        .icon("icons/plus.svg")
                        .w_auto()
                        .px_2()
                        .child(t!("sessions.new_button").to_string())
                        .on_click(move |_e, w, cx| {
                            on_new(w, cx);
                        }),
                ),
        )
        // --- Separator ---
        .child(div().h_px().bg(rgb(BORDER)).mx_4())
        // --- Hosts list (or empty state) ---
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_4()
                .py_2()
                .when_else(
                    hosts.is_empty(),
                    |el| {
                        el.flex().items_center().justify_center().child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_sm()
                                .child(t!("sessions.empty").to_string()),
                        )
                    },
                    |el| {
                        el.flex().flex_col().gap_1().children(hosts.iter().map(|h| {
                            let on_click = on_connect_rc.clone();
                            let on_edit = on_edit_rc.clone();
                            let on_remove = on_remove_rc.clone();
                            let host_id = h.id;
                            host_row(
                                h,
                                move |w, cx| on_click(host_id, w, cx),
                                move |w, cx| on_edit(host_id, w, cx),
                                move |w, cx| on_remove(host_id, w, cx),
                            )
                            .into_any_element()
                        }))
                    },
                ),
        )
        // --- Connection form overlay ---
        .when_some(form_state, |el, state| {
            el.child(ConnectionFormView::new(state, app))
        })
}

// ---------------------------------------------------------------------------
// Host row
// ---------------------------------------------------------------------------

fn host_row(
    host: &ConnectionHost,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    on_remove: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let row_id = ElementId::Name(format!("host-row-{}", host.id).into());
    let row_id_clone = row_id.clone();

    let edit_btn_id = ElementId::Name(format!("host-edit-{}", host.id).into());
    let remove_btn_id = ElementId::Name(format!("host-remove-{}", host.id).into());

    let edit_opacity_id = ElementId::Name(format!("host-edit-op-{}", host.id).into());
    let remove_opacity_id = ElementId::Name(format!("host-remove-op-{}", host.id).into());

    div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(BG_BASE))
        .on_double_click(move |_, w, cx| {
            gpui_animation::reset_transition(&row_id_clone);
            on_click(w, cx);
        })
        .with_transition(row_id)
        .transition_on_hover(Duration::from_millis(120), Linear, |hovered, s| {
            if *hovered {
                s.bg(rgb(SURFACE_ACTIVE))
            } else {
                s.bg(rgb(BG_BASE))
            }
        })
        // Host info (name + address)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(host.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(format!("{}@{}:{}", host.username, host.host, host.port)),
                ),
        )
        // Edit button (visible on hover)
        .child(
            div()
                .id(edit_opacity_id.clone())
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.)
                .child(
                    div()
                        .id(edit_btn_id)
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded_sm()
                        .cursor_pointer()
                        .child(
                            svg()
                                .path("icons/square-pen.svg")
                                .size_3p5()
                                .text_color(rgb(TEXT_MUTED)),
                        )
                        .on_click(move |_e, w, cx| {
                            on_edit(w, cx);
                            cx.stop_propagation();
                        }),
                )
                .with_transition(edit_opacity_id)
                .transition_on_hover(Duration::from_millis(100), Linear, |hovered, el| {
                    if *hovered {
                        el.opacity(0.7)
                    } else {
                        el.opacity(0.)
                    }
                }),
        )
        // Remove button (visible on hover)
        .child(
            div()
                .id(remove_opacity_id.clone())
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.)
                .child(
                    div()
                        .id(remove_btn_id)
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded_sm()
                        .cursor_pointer()
                        .child(
                            svg()
                                .path("icons/trash.svg")
                                .size_3p5()
                                .text_color(rgb(TEXT_MUTED)),
                        )
                        .on_click(move |_e, w, cx| {
                            on_remove(w, cx);
                            cx.stop_propagation();
                        }),
                )
                .with_transition(remove_opacity_id)
                .transition_on_hover(Duration::from_millis(100), Linear, |hovered, el| {
                    if *hovered {
                        el.opacity(0.7)
                    } else {
                        el.opacity(0.)
                    }
                }),
        )
}
