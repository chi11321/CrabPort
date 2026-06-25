use gpui::{prelude::FluentBuilder, *};
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crate::color::*;
use crate::layouts::connection_form::{
    ConnectionFormState, ConnectionKind, render_connection_form,
};
use gpui_component::input::InputState;

/// A saved connection host entry.
#[derive(Clone)]
pub struct ConnectionHost {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
}

/// Render the hosts sidebar view.
///
/// Shows a list of saved hosts with a "New" button at the top that opens
/// the connection creation form.
pub fn render_hosts_view(
    hosts: &[ConnectionHost],
    form_state: &ConnectionFormState,
    form_host: &Option<Entity<InputState>>,
    form_port: &Option<Entity<InputState>>,
    form_user: &Option<Entity<InputState>>,
    form_pass: &Option<Entity<InputState>>,
    on_close_form: impl Fn(&mut Window, &mut App) + 'static,
    on_connect: impl Fn(ConnectionKind, &mut Window, &mut App) + 'static,
    on_new: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
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
                        .child(t!("sidebar.hosts").to_string()),
                )
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(0x3b82f6))
                        .text_color(rgb(0xffffff))
                        .text_sm()
                        .child(t!("hosts.new_button").to_string())
                        .on_mouse_down(MouseButton::Left, move |_e, w, cx| {
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
                .when(hosts.is_empty(), |el| {
                    el.flex().items_center().justify_center().child(
                        div()
                            .text_color(rgb(TEXT_MUTED))
                            .text_sm()
                            .child(t!("hosts.empty").to_string()),
                    )
                })
                .when(!hosts.is_empty(), |el| {
                    el.flex()
                        .flex_col()
                        .gap_1()
                        .children(hosts.iter().map(|h| host_row(h)))
                }),
        )
        // --- Connection form overlay ---
        .child(render_connection_form(
            form_state,
            form_host,
            form_port,
            form_user,
            form_pass,
            on_close_form,
            on_connect,
        ))
}

// ---------------------------------------------------------------------------
// Host row
// ---------------------------------------------------------------------------

fn host_row(host: &ConnectionHost) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(BG_BASE))
        .hover(|el| el.bg(rgb(SURFACE_ACTIVE)))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
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
}
