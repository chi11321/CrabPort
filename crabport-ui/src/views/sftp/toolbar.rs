//! SFTP-tab-specific toolbar pieces.
//!
//! The SFTP tab shares the generic toolbar framework with the terminal
//! tab (see `crabport-ui/src/layouts/toolbar.rs` and
//! `crabport-ui/src/views/terminal/toolbar.rs`). The terminal toolbar
//! owns the "metrics + SFTP progress" slots; this module owns the
//! SFTP-tab-only actions that don't fit the slot model — currently just
//! the "transfer history" toggle button, which toggles the global
//! [`TransferHistoryController`] popover instead of a slot.

use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{duration_slower, EASE_STANDARD, RADIUS_SM};
use crate::views::sftp::TransferHistoryController;

// ---------------------------------------------------------------------------
// render_sftp_history_toggle
// ---------------------------------------------------------------------------

/// Render the "SFTP transfer history" toggle button shown in the SFTP
/// tab's toolbar (right-aligned, before the gear icon).
///
/// Ghost button mirroring the terminal pane's split buttons: default bg
/// is fully transparent so it blends with the toolbar background; the
/// `selected` state (i.e. the popover is currently open) tints the bg
/// and switches the icon to `text_primary()` so the user can tell at a
/// glance whether the popover is open.
///
/// On click the button calls `controller.toggle(event.position)` so the
/// popover anchors to the button. The controller is the global
/// [`TransferHistoryController`] entity held by `AppCtx` — same pattern
/// as the context-menu / tooltip controllers.
///
/// `on` is the controller's current open state, read by the caller
/// (which has a `&App` available). We take it as a param rather than
/// reading inside the element builder because `Entity::read_with`
/// needs a `&App` that isn't available at element-build time.
pub fn render_sftp_history_toggle(
    controller: Entity<TransferHistoryController>,
    on: bool,
) -> impl IntoElement {
    let btn_id = ElementId::Name("sftp-history-toggle".into());

    // Resting bg: transparent (alpha 0) so the toolbar background reads
    // through. The `selected` state swaps in `surface_hover()` with full
    // alpha and switches the icon to `text_primary()` so the user can
    // tell at a glance whether the popover is open.
    let rest_bg = rgba((surface_hover() << 8) | 0x00);
    let active_bg = rgba((surface_hover() << 8) | 0xFF);

    div()
        .id(btn_id.clone())
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded(RADIUS_SM)
        // Pre-set the rest bg so the transition registry has a concrete
        // `Some(bg)` to interpolate *from* on the first toggle.
        .bg(rest_bg)
        .with_transition(btn_id)
        .on_click(move |e, _w, cx| {
            // Toggle the popover, anchoring it to the click position.
            controller.update(cx, |c, cx| {
                c.toggle(e.position(), cx);
            });
            cx.stop_propagation();
        })
        // Drive bg through the transition system so the selected↔rest
        // toggle animates smoothly. No hover transition — the button
        // only reacts to the toggle state, not to mouse hover.
        .transition_when_else(
            on,
            duration_slower(),
            EASE_STANDARD,
            move |el| el.bg(active_bg),
            move |el| el.bg(rest_bg),
        )
        .child(
            svg()
                .path("icons/history.svg")
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(rgb(if on { text_primary() } else { text_muted() })),
        )
}
