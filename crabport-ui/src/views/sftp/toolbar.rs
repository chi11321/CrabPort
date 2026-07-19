//! SFTP-tab-specific toolbar pieces.
//!
//! The SFTP tab shares the generic toolbar framework with the terminal
//! tab (see `crabport-ui/src/layouts/toolbar.rs` and
//! `crabport-ui/src/views/terminal/toolbar.rs`). The terminal toolbar
//! owns the "metrics + SFTP progress" slots; this module owns the
//! SFTP-tab-only actions that don't fit the slot model — currently just
//! the "transfer history" toggle button, which flips a panel rather
//! than a slot.
//!
//! The toggle's state is persisted as
//! `appearance.terminal.toolbar.sftp_history` (a `bool`) so it survives
//! restarts and so the same ctxmenu that toggles the metrics slots also
//! toggles this one (via the shared `ToolbarVisibilityConfig::toggle`).

use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crate::color::*;
use crate::motion::{DURATION_SLOWER, EASE_STANDARD, RADIUS_SM};

// ---------------------------------------------------------------------------
// render_sftp_history_toggle
// ---------------------------------------------------------------------------

/// Render the "SFTP transfer history" toggle button shown in the SFTP
/// tab's toolbar (right-aligned, before the gear icon).
///
/// The button reflects the current `sftp_history` config flag: when on,
/// it's rendered in the accent color and acts as a "currently visible"
/// indicator; when off, it's muted. Clicking flips the flag and
/// persists it — the SFTP view reads the flag on its next render to
/// decide whether to draw the history panel.
///
/// The caller passes an `on_toggle` callback that handles both
/// (a) flipping the persisted config flag and (b) notifying the app so
/// the SFTP view repaints. This keeps the button self-contained: it
/// doesn't need a handle to the app entity.
pub fn render_sftp_history_toggle(on_toggle: impl Fn(&mut App) + 'static) -> impl IntoElement {
    // Read the current toggle state so the button reflects what the SFTP
    // view is actually showing. Cheap snapshot — no lock held across the
    // render boundary.
    let on = crabport_core::config::snapshot()
        .appearance
        .terminal
        .toolbar
        .sftp_history;

    let btn_id = ElementId::Name("sftp-history-toggle".into());

    div()
        .id(btn_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(24.0))
        .px_2()
        .rounded(RADIUS_SM)
        .cursor_pointer()
        .text_color(rgb(if on { text_primary() } else { text_muted() }))
        .bg(rgb(if on { surface_hover() } else { 0x00000000 }))
        .with_transition(btn_id)
        .transition_when_else(
            on,
            DURATION_SLOWER,
            EASE_STANDARD,
            |el| el.bg(rgb(surface_hover())).text_color(rgb(text_primary())),
            |el| el.bg(rgba(0x00000000)).text_color(rgb(text_muted())),
        )
        .on_click(move |_e, _w, cx| {
            // Delegate to the caller's callback — it flips the config
            // flag, persists, and notifies the app so the SFTP view and
            // this button both re-render with the new state.
            on_toggle(cx);
        })
        .child(
            svg()
                .path("icons/history.svg")
                .size(px(12.0))
                .flex_shrink_0(),
        )
        .child(
            div()
                .text_xs()
                .child(t!("toolbar.sftp_history").to_string()),
        )
}
