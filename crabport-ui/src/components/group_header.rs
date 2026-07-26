//! Shared collapsible group header used by the Hosts / Snippets / Tunnels
//! management views.
//!
//! Renders: chevron (animated rotation) + icon + name + member count +
//! optional favorite star. Clicking the header toggles collapse; clicking
//! the star toggles the group's favorite flag.
//!
//! A **virtual** "Favorites" group can be rendered by passing
//! `is_favorites_virtual = true`. In that case:
//! - The icon is a filled star instead of a folder.
//! - No per-group favorite star is shown (the group IS favorites).
//! - The `on_toggle_favorite` closure is ignored.
//! - The `on_context_menu` closure is ignored (the virtual group can't be
//!   renamed / deleted).

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use rust_i18n::t;
use std::rc::Rc;

use crate::color::*;
use crate::components::input::StyledInput;
use crate::motion::{EASE_LINEAR, RADIUS_MD, RADIUS_SM, duration_fast, duration_moderate};

/// Render a collapsible group header.
///
/// - `id_prefix` — unique prefix per view (e.g. `"host"`, `"snippet"`,
///   `"tunnel"`) so element ids don't collide when multiple views are
///   mounted in the same window.
/// - `group_id` — the numeric id of the group (used in element ids). For
///   the virtual Favorites group, pass `0`.
/// - `name` — display name.
/// - `member_count` — how many items are in the group.
/// - `is_collapsed` — whether the group body is currently collapsed.
/// - `favorite` — whether the group is starred (only meaningful for real
///   groups; ignored when `is_favorites_virtual`).
/// - `is_favorites_virtual` — when `true`, renders a star icon instead of
///   a folder and suppresses the per-group favorite toggle + context menu.
/// - `on_toggle_collapse` — fired when the header is clicked.
/// - `on_toggle_favorite` — fired when the star is clicked (`None` for the
///   virtual Favorites group).
/// - `on_context_menu` — fired on right-click with the mouse-down event,
///   so the caller can open a context menu (Rename / Delete Group) at the
///   cursor position. `None` for the virtual Favorites group (which can't
///   be renamed or deleted).
/// - `is_renaming` — when `true`, the group name is replaced by an inline
///   `StyledInput` backed by `rename_input` so the user can edit the name
///   in place (Enter commits, blur cancels — wired by the owning view).
///   `false` for the virtual Favorites group.
/// - `rename_input` — the `InputState` for the inline rename editor.
///   `Some` only when `is_renaming` is `true`; `None` otherwise.
pub fn group_header(
    id_prefix: &str,
    group_id: i64,
    name: impl Into<SharedString>,
    member_count: usize,
    is_collapsed: bool,
    favorite: bool,
    is_favorites_virtual: bool,
    on_toggle_collapse: Rc<dyn Fn(&mut Window, &mut App) + 'static>,
    on_toggle_favorite: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_context_menu: Option<Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>>,
    is_renaming: bool,
    rename_input: Option<Entity<InputState>>,
) -> impl IntoElement {
    let name: SharedString = name.into();
    let header_id = ElementId::Name(format!("{}-group-{}", id_prefix, group_id).into());
    let gid = group_id;
    // Chevron animation ID encodes the collapsed state so toggling
    // creates a fresh AnimationState and the rotation re-runs.
    let chevron_anim_id =
        ElementId::Name(format!("{}-group-chevron-{}-{}", id_prefix, gid, is_collapsed).into());

    div()
        .id(header_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .mt_2()
        .rounded(RADIUS_MD)
        // Pre-set the rest background so the transition registry has a
        // concrete `Some(bg)` to interpolate *from* on hover-in. Without
        // this the initial `to.bg` is `None` and the hover-in animation
        // jumps straight to `surface_hover` instead of easing in
        // (interpolating `None -> Some` returns `Some` immediately).
        .bg(rgb(bg_base()))
        .on_click({
            let on_toggle_collapse = on_toggle_collapse.clone();
            move |_e, w, cx| {
                on_toggle_collapse(w, cx);
            }
        })
        // Right-click context menu (Rename / Delete Group). Attached to the
        // raw div because `AnimatedWrapper` only re-exposes `on_hover` /
        // `on_click`, not `on_mouse_down`. Only wired for real groups —
        // the virtual Favorites group passes `None`.
        .when_some(on_context_menu, |el, on_ctx| {
            el.on_mouse_down(MouseButton::Right, move |event, w, cx| {
                on_ctx(event, w, cx);
            })
        })
        .with_transition(header_id)
        .transition_on_hover(duration_fast(), EASE_LINEAR, move |hovered, el| {
            if *hovered {
                el.bg(rgb(surface_hover()))
            } else {
                el.bg(rgb(bg_base()))
            }
        })
        // Chevron: animates 90° rotation on collapse/expand. Hidden while
        // renaming so the input has room and the row reads as an editor.
        .when(!is_renaming, |el| {
            el.child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size_3()
                    .text_color(rgb(text_muted()))
                    .with_animation(
                        chevron_anim_id,
                        Animation::new(duration_moderate()).with_easing(ease_in_out),
                        move |this, delta| {
                            // Collapsed: 0 -> -90° (points right).
                            // Open: -90° -> 0° (points down).
                            let angle = if is_collapsed {
                                -delta * std::f32::consts::FRAC_PI_2
                            } else {
                                -(1.0 - delta) * std::f32::consts::FRAC_PI_2
                            };
                            this.with_transformation(Transformation::rotate(radians(angle)))
                        },
                    ),
            )
        })
        // Icon: star for the virtual Favorites group, folder for real
        // groups. Hidden while renaming.
        .when(!is_renaming, |el| {
            el.child(if is_favorites_virtual {
                svg()
                    .path("icons/star.svg")
                    .size_4()
                    .text_color(rgb(term_yellow()))
                    .into_any_element()
            } else {
                svg()
                    .path("icons/folder.svg")
                    .size_4()
                    .text_color(rgb(text_muted()))
                    .into_any_element()
            })
        })
        // Name — replaced by an inline `StyledInput` while renaming (mirrors
        // the SFTP inline-rename pattern). The owning view owns the
        // `InputState` and wires Enter (commit) / blur (cancel); here we
        // just render it in place of the label. Stop click propagation so a
        // drag-select inside the input doesn't toggle collapse.
        .when_some(rename_input, |el, input| {
            el.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        StyledInput::new(format!("{}-group-rename-{}", id_prefix, gid), input)
                            .xsmall(),
                    ),
            )
        })
        .when(!is_renaming, |el| {
            el.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(text_primary()))
                    .child(name),
            )
        })
        // Member count — hidden while renaming.
        .when(!is_renaming, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(t!("groups.member_count", count = member_count).to_string()),
            )
        })
        // Favorite star toggle (far right). Only for real groups — the
        // virtual Favorites group doesn't have its own favorite flag. Hidden
        // while renaming so the input has the full row width.
        .when_some(on_toggle_favorite, |el, on_fav| {
            if is_renaming {
                return el;
            }
            let star_id = ElementId::Name(format!("{}-group-star-{}", id_prefix, gid).into());
            el.child(
                div()
                    .id(star_id.clone())
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS_SM)
                    .child(
                        svg()
                            .path("icons/star.svg")
                            .size_4()
                            .text_color(rgb(if favorite {
                                term_yellow()
                            } else {
                                text_muted()
                            })),
                    )
                    .with_transition(star_id)
                    .transition_when_else(
                        favorite,
                        duration_fast(),
                        EASE_LINEAR,
                        |el| el.opacity(1.0),
                        |el| el.opacity(0.0),
                    )
                    .on_click({
                        let on_fav = on_fav.clone();
                        move |_e, w, cx| {
                            on_fav(w, cx);
                        }
                    }),
            )
        })
}
