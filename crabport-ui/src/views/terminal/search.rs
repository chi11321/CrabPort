//! Terminal search overlay.
//!
//! Rendered as a layout-level row (between the terminal body and the toolbar)
//! so that opening it pushes the toolbar — and its split / panel buttons —
//! down with a smooth height animation. The bar slides open from the
//! top-right corner using `gpui-animation`'s `transition_when_else` with
//! `EASE_OUT` (starts fast, settles) on open and `EASE_STANDARD` on close.
//!
//! Keyboard (while the search input is focused):
//!   - **Enter** — next match
//!   - **Shift+Enter** — previous match
//!   - **Escape** — close the search bar

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, Size};
use rust_i18n::t;

use crate::color::*;
use crate::motion::{EASE_OUT, RADIUS_MD, duration_slow};

/// Height of the search bar row (px). Matches the split/panel button
/// height (24px) plus a small gap so the search bar feels like it belongs
/// to the same cluster.
const SEARCH_BAR_HEIGHT: f32 = 28.0;

/// A search-direction hint passed to the terminal view so it can advance or
/// rewind the active match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Next,
    Prev,
}

/// Render the search bar as an animated-height row.
///
/// Pass `visible = true` to slide it open; `false` to collapse it. The
/// height animates between `0` and `SEARCH_BAR_HEIGHT` so sibling layout
/// (the toolbar below) is pushed down / pulled back up smoothly.
///
/// `active`/`total` form the "3 / 17" counter.
///
/// `on_direction` is called with `Next`/`Prev` when the user clicks the
/// arrows or presses Enter / Shift+Enter. `on_close` is called on
/// Escape or the close button.
pub fn render_search_bar<
    D: Fn(&SearchDirection, &mut Window, &mut App) + 'static,
    C: Fn(&mut Window, &mut App) + 'static,
>(
    state: &Entity<InputState>,
    visible: bool,
    active: Option<usize>,
    total: usize,
    on_direction: D,
    on_close: C,
) -> impl IntoElement + use<D, C> {
    let on_direction = Rc::new(on_direction);
    let on_close = Rc::new(on_close);

    let counter = if total == 0 {
        t!("terminal.search.no_matches").to_string()
    } else {
        let idx = active.map(|i| i + 1).unwrap_or(1);
        format!("{} / {}", idx, total)
    };

    let content = div()
        .h(px(SEARCH_BAR_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .pl_2()
        .pr_1()
        .mr_2()
        .rounded(RADIUS_MD)
        .bg(rgb(input_bg_focused()))
        .border_1()
        .border_color(rgb(input_border_focused()))
        .shadow_sm()
        // Search icon prefix.
        .child(
            svg()
                .path("icons/search.svg")
                .size_3p5()
                .text_color(rgb(text_muted())),
        )
        // Query input.
        .child(
            div().flex_1().min_w(px(120.0)).child(
                Input::new(state)
                    .appearance(false)
                    .bordered(false)
                    .with_size(Size::XSmall),
            ),
        )
        // Match counter.
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .min_w(px(36.0))
                .flex()
                .items_center()
                .justify_center()
                .child(counter),
        )
        // Prev button.
        .child(
            div()
                .id("search-prev")
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .text_color(rgb(text_muted()))
                .hover(|s| s.bg(rgb(surface_hover())))
                .on_click({
                    let on_dir = on_direction.clone();
                    move |_e, w, cx| on_dir(&SearchDirection::Prev, w, cx)
                })
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size_3()
                        .text_color(rgb(text_muted()))
                        .with_transformation(
                            gpui::Transformation::default()
                                .with_rotation(gpui::radians(std::f32::consts::PI)),
                        ),
                ),
        )
        // Next button.
        .child(
            div()
                .id("search-next")
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .text_color(rgb(text_muted()))
                .hover(|s| s.bg(rgb(surface_hover())))
                .on_click({
                    let on_dir = on_direction.clone();
                    move |_e, w, cx| on_dir(&SearchDirection::Next, w, cx)
                })
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size_3()
                        .text_color(rgb(text_muted())),
                ),
        )
        // Close button.
        .child(
            div()
                .id("search-close")
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .text_color(rgb(text_muted()))
                .hover(|s| s.bg(rgb(surface_hover())))
                .on_click({
                    let on_close = on_close.clone();
                    move |_e, w, cx| on_close(w, cx)
                })
                .child(
                    svg()
                        .path("icons/close.svg")
                        .size_3()
                        .text_color(rgb(text_muted())),
                ),
        )
        .on_key_down({
            let on_close = on_close.clone();
            move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    on_close(window, cx);
                }
            }
        });

    // Outer container: right-aligned, animates height between 0 and
    // SEARCH_BAR_HEIGHT. The inner content is always rendered so the
    // transition has something to reveal / clip.
    //
    // We use `w_auto` + `overflow_visible` on the outer (not `overflow_hidden`)
    // so the search bar can be wider than the button cluster below it —
    // the content extends leftward past the button column.
    //
    // Open: `duration_slow` (250ms) on `EASE_OUT` — slides in quickly and
    //   settles.
    // Close: `duration_moderate` (200ms) on `EASE_STANDARD` — symmetric
    //   accel/decel.
    //
    // First-render fix: the `with_transition` cache needs an initial render
    // to establish the "closed" state. We set `h(0)` + `opacity(0)` as the
    // static base so the very first frame after mount is already closed;
    // `transition_when_else` then animates *from* that base toward the
    // open state, giving the first toggle its animation.
    div()
        .id("terminal-search-row")
        .flex()
        .flex_col()
        .items_end()
        .overflow_hidden()
        .flex_shrink_0()
        .h(px(0.0))
        .opacity(0.0)
        .when(visible, |el| el.occlude())
        .with_transition("terminal-search-row-height")
        .transition_when_else(
            visible,
            duration_slow(),
            EASE_OUT,
            move |el| el.h(px(SEARCH_BAR_HEIGHT)).opacity(1.0),
            move |el| el.h(px(0.0)).opacity(0.0),
        )
        .child(content)
}
