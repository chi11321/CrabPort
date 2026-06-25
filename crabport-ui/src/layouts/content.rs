use std::collections::HashMap;
use std::rc::Rc;

use gpui::*;

use crate::app::{SidebarItem, Tab, TabKind};
use crate::color::*;
use crate::layouts::tabbar::render_tab_bar;
use crate::views;
use crate::views::terminal::TerminalView;

pub fn render_content(
    selected: SidebarItem,
    handle: &Entity<crate::app::CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    terminal_views: &HashMap<u64, Entity<TerminalView>>,
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

    let view: AnyElement = match active_tab.map(|t| t.kind) {
        Some(TabKind::Home) => match selected {
            SidebarItem::Hosts => views::hosts::render_hosts_view().into_any_element(),
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
                // Auto-focus the terminal view for keyboard input
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
