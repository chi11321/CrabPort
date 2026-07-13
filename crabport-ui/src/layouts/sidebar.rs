use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutCubic};

use crate::app::SidebarItem;
use crate::color::*;
use crate::components::button::Button;

pub fn render_sidebar(
    selected: SidebarItem,
    show: bool,
    handle: &Entity<crate::app::CrabportApp>,
) -> impl IntoElement {
    div()
        .id("sidebar-container")
        .h_full()
        .flex_shrink_0()
        .overflow_x_hidden()
        .w(px(180.0))
        .with_transition("sidebar-container")
        .transition_when_else(
            show,
            std::time::Duration::from_millis(300),
            EaseInOutCubic,
            |el| el.w(px(180.0)),
            |el| el.w_0(),
        )
        .child(
            div()
                .h_full()
                .border_r_1()
                .border_color(rgb(border()))
                .bg(rgb(bg_sidebar()))
                .flex()
                .flex_col()
                .pt(px(if cfg!(target_os = "macos") { 44.0 } else { 8.0 }))
                .px_2()
                .gap_2()
                .children(SidebarItem::all().map(|item| {
                    let is_selected = item == selected;
                    let h = handle.clone();
                    Button::new(ElementId::Name(format!("sidebar-{item:?}").into()))
                        .tab()
                        .selected(is_selected)
                        .icon(item.icon())
                        .child(item.label())
                        .on_click(move |_e, _w, cx| {
                            h.update(cx, |app, _| {
                                app.sidebar_item = item;
                            });
                        })
                        .h_9()
                        .border_0()
                        .px_2()
                        .text_sm()
                })),
        )
}
