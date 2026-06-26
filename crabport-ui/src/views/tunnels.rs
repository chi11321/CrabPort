use gpui::*;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crate::color::*;
use crate::components::button::Button;

/// Render the tunnels sidebar view.
pub fn render_tunnels_view(on_new: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
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
                        .child(t!("sidebar.tunnels").to_string()),
                )
                .child(
                    Button::new("tunnels-new-btn")
                        .primary()
                        .icon("icons/plus.svg")
                        .w_auto()
                        .px_2()
                        .child(t!("tunnels.new_button").to_string())
                        .on_click(move |_e, w, cx| {
                            on_new(w, cx);
                        }),
                ),
        )
        // --- Separator ---
        .child(div().h_px().bg(rgb(BORDER)).mx_4())
        // --- Placeholder ---
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_4()
                .py_2()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_sm()
                        .child(t!("tunnels.empty").to_string()),
                ),
        )
}
