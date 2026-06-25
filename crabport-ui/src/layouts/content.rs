use std::collections::HashMap;
use std::rc::Rc;

use gpui::*;
use gpui_component::input::InputState;

use crate::app::{CrabportApp, SidebarItem, Tab, TabKind};
use crate::color::*;
use crate::layouts::connection_form::{ConnectionFormState, ConnectionKind};
use crate::layouts::tabbar::render_tab_bar;
use crate::views;
use crate::views::hosts::ConnectionHost;
use crate::views::terminal::TerminalView;

pub fn render_content(
    selected: SidebarItem,
    handle: &Entity<CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    terminal_views: &HashMap<u64, Entity<TerminalView>>,
    hosts: &[ConnectionHost],
    form_state: &ConnectionFormState,
    form_host: &Option<Entity<InputState>>,
    form_port: &Option<Entity<InputState>>,
    form_user: &Option<Entity<InputState>>,
    form_pass: &Option<Entity<InputState>>,
    window: &mut Window,
    cx: &App,
) -> Div {
    let active_tab = tabs.iter().find(|t| t.id == active_tab_id);
    let handle_c = handle.clone();
    let on_close: Rc<dyn Fn(u64, &mut Window, &mut App)> = Rc::new(move |id, _w, cx| {
        handle_c.update(cx, |app, cx| {
            app.close_tab(id, cx);
        });
    });

    let handle_form = handle.clone();
    let on_close_form = move |_: &mut Window, cx: &mut App| {
        handle_form.update(cx, |app, cx| {
            app.connection_form.active = false;
            cx.notify();
        });
    };

    let handle_connect = handle.clone();
    let on_connect = move |_kind: ConnectionKind, _: &mut Window, cx: &mut App| {
        handle_connect.update(cx, |app, cx| {
            let host = app
                .form_host_input
                .as_ref()
                .map(|s| s.read(cx).text().to_string())
                .unwrap_or_default();
            let port = app
                .form_port_input
                .as_ref()
                .map(|s| s.read(cx).text().to_string())
                .unwrap_or_else(|| "22".into());
            let username = app
                .form_user_input
                .as_ref()
                .map(|s| s.read(cx).text().to_string())
                .unwrap_or_default();
            let password = app
                .form_pass_input
                .as_ref()
                .map(|s| s.read(cx).text().to_string())
                .unwrap_or_default();
            let port_num: u16 = port.parse().unwrap_or(22);
            let name = format!("{}@{}", username, host);
            app.hosts.push(ConnectionHost {
                name,
                host: host.to_string(),
                port: port_num,
                username: username.to_string(),
            });
            app.connection_form.active = false;
            app.add_ssh_tab(&host, port_num, &username, &password, cx);
            cx.notify();
        });
    };

    let handle_new = handle.clone();
    let on_new = move |_: &mut Window, cx: &mut App| {
        handle_new.update(cx, |app, cx| {
            app.connection_form.active = true;
            cx.notify();
        });
    };

    let view: AnyElement = match active_tab.map(|t| t.kind) {
        Some(TabKind::Home) => match selected {
            SidebarItem::Hosts => views::hosts::render_hosts_view(
                hosts,
                form_state,
                form_host,
                form_port,
                form_user,
                form_pass,
                on_close_form,
                on_connect,
                on_new,
            )
            .into_any_element(),
            SidebarItem::Credentials => {
                views::credentials::render_credentials_view().into_any_element()
            }
            SidebarItem::Snippets => views::snippets::render_snippets_view().into_any_element(),
            SidebarItem::History => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .child(selected.label().to_string()),
                )
                .into_any_element(),
        },
        Some(TabKind::Terminal) => {
            if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
                terminal_entity.read_with(cx, |view, cx| {
                    window.focus(&view.focus_handle(cx));
                });

                div()
                    .size_full()
                    .m_2()
                    .child(terminal_entity.clone())
                    .into_any_element()
            } else {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(TEXT_MUTED)).child("Terminal"))
                    .into_any_element()
            }
        }
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(div().text_color(rgb(TEXT_MUTED)).child("No tab"))
            .into_any_element(),
    };

    div()
        .flex_1()
        .h_full()
        .bg(rgb(BG_BASE))
        .flex()
        .flex_col()
        .child(render_tab_bar(
            handle,
            tabs,
            active_tab_id,
            active_tab.map(|t| t.kind == TabKind::Home).unwrap_or(false),
            on_close,
        ))
        .child(view)
}
