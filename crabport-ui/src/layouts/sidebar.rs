use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::app::SidebarItem;
use crate::color::*;
use crate::components::button::Button;
use crate::motion::{EASE_STANDARD, duration_slower};

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
            duration_slower(),
            EASE_STANDARD,
            |el| el.w(px(180.0)),
            |el| el.w_0(),
        )
        .child(
            div()
                .h_full()
                .border_r_1()
                .border_color(rgb(border()))
                .bg(sidebar_bg_color())
                .flex()
                .flex_col()
                .pt(px(if cfg!(target_os = "macos") {
                    44.0
                } else {
                    12.0
                }))
                .px_2()
                .gap_2()
                // macOS reserves the top 44px for the native traffic lights;
                // the wordmark is only needed to fill the otherwise-empty top
                // on Windows/Linux where there are no native controls there.
                .when(cfg!(not(target_os = "macos")), |el| {
                    el.child(wordmark_header())
                })
                .children(SidebarItem::all().map(|item| {
                    let is_selected = item == selected;
                    let h = handle.clone();
                    Button::new(ElementId::Name(format!("sidebar-{item:?}").into()))
                        .tab()
                        // Overide the .tab() default bg with the vibrancy-aware color:
                        // fully transparent on macOS so the sidebar 毛玻璃 reads
                        // through the button; hover / selected keep their colors.
                        .bg(tab_btn_bg_color())
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

/// Compact app wordmark header at the top of the sidebar.
///
/// A small terminal-prompt mark + `CRABPORT` wordmark in a ~32px row, with a
/// hairline divider below. Sits below the macOS traffic-light padding on
/// macOS (so it isn't hidden under the native buttons) and at the very top
/// on Windows/Linux to fix the otherwise-empty sidebar top. Purely visual —
/// no `on_click`.
fn wordmark_header() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            // Wordmark row — height matches the nav buttons' visual weight.
            div()
                .h_8()
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    // Crab emoji as the brand mark — Crabport = Crab + port.
                    // No `.text_color` override so the emoji keeps its native
                    // color rendering (text_color would tint emoji glyphs).
                    div()
                        .text_base()
                        .flex_shrink_0()
                        .child("\u{1F980}".to_string()),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(text_muted()))
                        .child("CRABPORT"),
                ),
        )
        .child(
            // Hairline divider below the wordmark, inset to align with the
            // button padding (.px_2 on the parent).
            div().h_px().mx_2().bg(rgb(border())).w_full(),
        )
}
