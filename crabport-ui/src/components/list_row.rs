//! Shared builders for the full-page sidebar list views (sessions /
//! snippets / tunnels): row containers with hover easing, favorite stars,
//! and animated group-collapse bodies.
//!
//! All three grouped views render the exact same chrome; only the row
//! *content* and the mouse handlers differ, so those come in as closures
//! while the ids, spacing, and transitions live here once.

use std::rc::Rc;

use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{EASE_STANDARD, RADIUS_MD, duration_fast, duration_moderate};

/// Standard list-row container: the shared layout chain, then the caller's
/// interactive handlers (`decorate` — attached *before* `with_transition`
/// because the `AnimatedWrapper` doesn't forward `on_mouse_down` /
/// `on_double_click`), then hover tracking that writes into the owning
/// view's hover slot, then the eased background highlight, then `children`.
///
/// `key` is `(item_id, is_favorite_copy)` for the grouped views so the
/// favorites copy of an item and its real-group copy don't cross-highlight.
pub fn interactive_row<V: 'static>(
    row_id: ElementId,
    is_highlighted: bool,
    entity: WeakEntity<V>,
    key: (i64, bool),
    hover_slot: fn(&mut V) -> &mut Option<(i64, bool)>,
    decorate: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
    children: Vec<AnyElement>,
) -> impl IntoElement {
    let base = div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(RADIUS_MD)
        .bg(rgb(bg_base()));
    decorate(base)
        .with_transition(row_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                let slot = hover_slot(view);
                if *hovered {
                    *slot = Some(key);
                } else if *slot == Some(key) {
                    *slot = None;
                }
                cx.notify();
            });
        })
        .transition_when_else(
            is_highlighted,
            duration_fast(),
            EASE_STANDARD,
            |el| el.bg(rgb(surface_active())),
            |el| el.bg(rgb(bg_base())),
        )
        .children(children)
}

/// Favorite star toggle at the far right of a row. Fades in on hover;
/// stays visible (yellow) when already favorited so the user can see +
/// unstar.
pub fn favorite_star(
    id_prefix: &str,
    item_id: i64,
    is_favorite: bool,
    visible: bool,
    on_toggle: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let star_id = ElementId::Name(format!("{}-star-{}", id_prefix, item_id).into());
    div()
        .id(star_id.clone())
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path("icons/star.svg")
                .size_4()
                .text_color(rgb(if is_favorite {
                    term_yellow()
                } else {
                    text_muted()
                })),
        )
        .with_transition(star_id)
        .transition_when_else(
            visible,
            duration_fast(),
            EASE_STANDARD,
            |el| el.opacity(1.0),
            |el| el.opacity(0.0),
        )
        .on_click(move |_e, _w, cx| {
            on_toggle(cx);
        })
}

/// Animated group body: eases height + opacity between collapsed (0) and
/// expanded (`row_count * row_height - 4`). Rows are always built (even
/// when collapsed) so the expand animation has content to reveal.
pub fn collapsible_group_body(
    id_prefix: &str,
    group_id: i64,
    expanded: bool,
    row_count: usize,
    row_height: f32,
    rows: Vec<AnyElement>,
) -> AnyElement {
    let body_id = ElementId::Name(format!("{}-group-body-{}", id_prefix, group_id).into());
    div()
        .id(body_id.clone())
        .flex()
        .flex_col()
        .gap_1()
        .overflow_hidden()
        .with_transition(body_id)
        .transition_when_else(
            expanded,
            duration_moderate(),
            EASE_STANDARD,
            move |el| el.h(px(row_count as f32 * row_height - 4.0)).opacity(1.0),
            |el| el.h(px(0.0)).opacity(0.0),
        )
        .children(rows)
        .into_any_element()
}

/// Wrap an `Option<Rc<dyn Fn(i64, ...)>>` callback with a fixed id into a
/// plain `Fn(&mut Window, &mut App)` closure — the shape every row builder
/// needs for its menu items.
pub fn bind_id_callback(
    cb: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    id: i64,
) -> impl Fn(&mut Window, &mut App) + 'static {
    move |w, cx| {
        if let Some(ref cb) = cb {
            cb(id, w, cx);
        }
    }
}
