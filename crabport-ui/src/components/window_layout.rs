//! Shared layout primitives for auxiliary windows that use a narrow
//! tab-button sidebar on the left + a content pane on the right
//! (Settings, About).
//!
//! Both windows render an almost-identical sidebar and root layout.
//! [`render_sidebar_window`] + [`render_tab_sidebar`] encapsulate that
//! pattern so each window only supplies its tab list + content element.

use gpui::*;

use crate::color::*;
use crate::components::button::Button;

/// Render the root layout: full-size bg → sidebar (left) → content (right).
///
/// `sidebar_width` lets each window pick its own width (Settings uses 180px,
/// About uses 160px). The content pane is `flex_1` + `overflow_hidden`.
pub fn render_sidebar_window(sidebar: impl IntoElement, content: impl IntoElement) -> Div {
    div()
        .size_full()
        .bg(rgb(bg_base()))
        .flex()
        .flex_row()
        .relative()
        .child(sidebar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .child(content),
        )
}

/// One tab entry in a sidebar — a label + an icon path (optional).
pub struct SidebarTabEntry {
    pub id: ElementId,
    pub label: SharedString,
    pub icon: Option<&'static str>,
}

/// Render a vertical sidebar of tab buttons.
///
/// - `id_prefix` — unique per window (e.g. `"settings"`, `"about"`) so element
///   ids don't collide.
/// - `width` — sidebar width in px.
/// - `entries` — one per tab.
/// - `selected_idx` — index of the currently-active tab.
/// - `on_select` — called with the tab index when the user clicks a tab.
pub fn render_tab_sidebar(
    entries: Vec<SidebarTabEntry>,
    width: Pixels,
    selected_idx: usize,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = std::rc::Rc::new(on_select);
    div()
        .h_full()
        .w(width)
        .flex_shrink_0()
        .border_r_1()
        .border_color(rgb(border()))
        .bg(rgb(bg_sidebar()))
        .flex()
        .flex_col()
        .pt(px(if cfg!(target_os = "macos") { 44.0 } else { 8.0 }))
        .px_2()
        .gap_2()
        .children(entries.into_iter().enumerate().map(|(i, entry)| {
            let is_selected = i == selected_idx;
            let on_select = on_select.clone();
            let mut btn = Button::new(entry.id)
                .tab()
                .selected(is_selected)
                .child(entry.label)
                .on_click(move |_e, w, cx| {
                    on_select(i, w, cx);
                })
                .h_9()
                .border_0()
                .px_2()
                .text_sm()
                .justify_start();
            if let Some(icon) = entry.icon {
                btn = btn.icon(icon);
            }
            btn
        }))
}
