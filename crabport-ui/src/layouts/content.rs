use std::collections::HashMap;
use std::rc::Rc;

use gpui::*;

use crate::app::{CrabportApp, SidebarItem, Tab, TabKind};
use crate::color::*;
use crate::layouts::connection_form::ConnectionFormState;
use crate::layouts::tabbar::render_tab_bar;
use crate::layouts::terminal_toolbar::render_terminal_toolbar;
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
    form_entity: Option<&ConnectionFormState>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let active_tab = tabs.iter().find(|t| t.id == active_tab_id);
    let handle_c = handle.clone();
    let on_close: Rc<dyn Fn(u64, &mut Window, &mut App)> = Rc::new(move |id, _w, cx| {
        handle_c.update(cx, |app, cx| {
            app.close_tab(id, cx);
        });
    });

    let app_handle = handle.clone();
    let on_new = move |w: &mut Window, cx: &mut App| {
        app_handle.update(cx, |app, cx| {
            app.open_connection_form(w, cx);
        });
    };

    let view: AnyElement = match active_tab.map(|t| t.kind) {
        Some(TabKind::Home) => match selected {
            SidebarItem::Sessions => {
                let app_handle = handle.clone();
                let on_connect = move |host_id: i64, _w: &mut Window, cx: &mut App| {
                    app_handle.update(cx, |app, cx| {
                        app.connect_to_host(host_id, cx);
                    });
                };
                let app_handle_edit = handle.clone();
                let on_edit = move |host_id: i64, w: &mut Window, cx: &mut App| {
                    app_handle_edit.update(cx, |app, cx| {
                        app.edit_host(host_id, w, cx);
                    });
                };
                let app_handle_remove = handle.clone();
                let on_remove = move |host_id: i64, _w: &mut Window, cx: &mut App| {
                    app_handle_remove.update(cx, |app, cx| {
                        app.remove_host(host_id, cx);
                    });
                };
                views::hosts::render_hosts_view(
                    hosts,
                    form_entity,
                    handle.clone(),
                    on_new,
                    on_connect,
                    on_edit,
                    on_remove,
                )
                .into_any_element()
            }
            SidebarItem::Tunnels => {
                views::tunnels::render_tunnels_view(|_, _| {}).into_any_element()
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
                    .key_context("Terminal")
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

    let is_terminal = active_tab
        .map(|t| t.kind == TabKind::Terminal)
        .unwrap_or(false);

    // Read monitor status & metrics from the active TerminalView's backend
    let (status, metrics) = if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
            terminal_entity.read_with(cx, |view, _cx| {
                if let Some(m) = view.monitor() {
                    (m.status(), m.metrics())
                } else {
                    (
                        crabport_terminal::terminal::RemoteStatus::Local,
                        crabport_terminal::terminal::RemoteMetrics::default(),
                    )
                }
            })
        } else {
            (
                crabport_terminal::terminal::RemoteStatus::Local,
                crabport_terminal::terminal::RemoteMetrics::default(),
            )
        }
    } else {
        (
            crabport_terminal::terminal::RemoteStatus::Local,
            crabport_terminal::terminal::RemoteMetrics::default(),
        )
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
        .child(render_terminal_toolbar(is_terminal, status, metrics))
}
