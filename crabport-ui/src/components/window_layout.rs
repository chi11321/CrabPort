//! Shared layout primitives for auxiliary windows that use a narrow
//! tab-button sidebar on the left + a content pane on the right
//! (Settings, About).
//!
//! Both windows render an almost-identical sidebar and root layout.
//! [`render_sidebar_window`] + [`render_tab_sidebar`] encapsulate that
//! pattern so each window only supplies its tab list + content element.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::color::*;
use crate::components::button::Button;

/// Render the root layout: full-size bg → sidebar (left) → content (right).
///
/// `sidebar_width` lets each window pick its own width (Settings uses 180px,
/// About uses 160px). The content pane is `flex_1` + `overflow_hidden`.
///
/// On Windows/Linux a `h_11` overlay at the top of the **content pane**
/// (right of the sidebar — not full-width) is registered as a window-drag
/// region (see [`WindowControlArea::Drag`]) so the user can move the window
/// by grabbing any empty area of that strip, mirroring a native title bar.
/// The window-control buttons (rendered by each window on top of this strip)
/// call [`InteractiveElement::occlude_mouse`] so they keep working under
/// Windows' `WM_NCHITTEST`. macOS is excluded — it already provides drag via
/// the transparent system title bar.
pub fn render_sidebar_window(sidebar: impl IntoElement, content: impl IntoElement) -> Div {
    div()
        .size_full()
        // macOS: leave the root transparent so the window's vibrancy layer
        // reads through the sidebar (which paints a translucent tint). The
        // content pane caller supplies an opaque background to mask vibrancy
        // outside the sidebar.
        .when(cfg!(not(target_os = "macos")), |el| el.bg(rgb(bg_base())))
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
                .relative()
                .when(cfg!(target_os = "macos"), |el| el.bg(opaque_base_bg()))
                .child(content)
                // Top drag strip (Windows/Linux only), scoped to the
                // content pane so the sidebar stays fully interactive.
                // Painted behind the window-control buttons that each
                // window overlays on top of it; those buttons
                // `occlude_mouse` so their clicks win over this drag region
                // under Windows' `WM_NCHITTEST`.
                //
                // `on_mouse_down`/`on_mouse_up` are only used on Linux
                // (via `start_window_move` / `toggle_maximize`) — Windows
                // handles drag + double-click-maximize natively once
                // `WM_NCHITTEST` returns `HTCAPTION`. macOS isn't reached
                // at all (see the `cfg` below).
                .when(cfg!(not(target_os = "macos")), |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h_11()
                            .window_control_area(WindowControlArea::Drag)
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_: &MouseDownEvent, window, _cx| {
                                    crate::components::window_controls::start_window_move(window);
                                },
                            )
                            .on_mouse_up(MouseButton::Left, |event: &MouseUpEvent, window, _| {
                                if event.click_count == 2 {
                                    crate::components::window_controls::toggle_maximize(window);
                                }
                            }),
                    )
                }),
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
        .bg(sidebar_bg_color())
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
                .bg(tab_btn_bg_color())
                .selected(is_selected)
                .child(entry.label)
                .on_click(move |_e, w, cx| {
                    on_select(i, w, cx);
                })
                .h_9()
                .border_0()
                .px_2()
                .text_sm()
                .justify_start()
                // Block the top drag strip (see `render_sidebar_window`)
                // beneath this sidebar tab so clicking the tab switches
                // panes instead of starting a window drag on Windows/Linux.
                .occlude_mouse();
            if let Some(icon) = entry.icon {
                btn = btn.icon(icon);
            }
            btn
        }))
}
