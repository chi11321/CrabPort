//! Shared overlay rendering helpers.
//!
//! Several dialogs and forms (alert, group form, connection form, snippet
//! form, tunnel form) render a near-identical dimmed backdrop overlay with
//! an animated fade-in/out. [`render_overlay`] encapsulates that pattern so
//! each call site passes just its id, open flag, on-close callback, and
//! dialog content.

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};

/// Render a full-screen dimmed overlay that fades in/out when `open`
/// toggles. The overlay occludes + captures clicks while open (so a
/// backdrop click fires `on_close`) and does nothing while hidden so it
/// doesn't block interaction.
///
/// `overlay_id` must be unique per dialog instance — each caller uses a
/// fixed name (e.g. `"conn-form-overlay"`, `"alert-overlay"`).
pub fn render_overlay(
    overlay_id: impl Into<ElementId>,
    open: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    child: impl IntoElement,
) -> impl IntoElement {
    let overlay_id = overlay_id.into();
    div()
        .id(overlay_id.clone())
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000000))
        .when(open, |el| {
            el.occlude().on_click(move |_e, w, cx| {
                if let Some(ref cb) = on_close {
                    cb(w, cx);
                }
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            open,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(0x00000080)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(child)
}
