//! # Tooltip
//!
//! A global, reusable tooltip overlay with hover fade-in/out easing. Like
//! [`ContextMenuController`], it's an `Entity` held by the app root and
//! rendered as a top-level child.
//!
//! Trigger it from any element's `on_hover`:
//!
//! ```ignore
//! .on_hover(move |hovered, w, cx| {
//!     tooltip.update(cx, |t, cx| {
//!         if *hovered {
//!             t.show("My tooltip".to_string(), w.mouse_position(), cx);
//!         } else {
//!             t.hide(cx);
//!         }
//!     });
//! })
//! ```
//!
//! The tooltip fades in/out with a 120ms opacity transition. A 400ms show
//! delay keeps it from flickering on quick mouse-overs.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{DURATION_BASE, EASE_STANDARD, RADIUS_SM};

/// How long the fade-out animation runs before the state is dropped. Should
/// match the `transition_when_else` duration used in `render_tooltip`.
const TOOLTIP_DISMISS_MS: u64 = 150;
/// Delay before showing (ms). Keeps the tooltip from flickering on quick
/// mouse-overs.
const TOOLTIP_SHOW_DELAY_MS: u64 = 400;

pub struct TooltipController {
    state: Option<TooltipState>,
    generation: u64,
}

#[derive(Clone)]
struct TooltipState {
    text: SharedString,
    position: Point<Pixels>,
    open: bool,
}

impl TooltipController {
    pub fn new() -> Self {
        Self {
            state: None,
            generation: 0,
        }
    }

    /// Show a tooltip at `position` after a short delay. Any currently-
    /// showing tooltip is replaced.
    pub fn show(&mut self, text: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let gen_id = self.generation;

        let entity = cx.entity().downgrade();
        let text: SharedString = text.into();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(TOOLTIP_SHOW_DELAY_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                if this.generation != gen_id {
                    return;
                }
                gpui_animation::reset_transition(&ElementId::Name("crabport-tooltip".into()));
                this.state = Some(TooltipState {
                    text: text.clone(),
                    position,
                    open: true,
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Begin the fade-out animation. After the transition duration, the
    /// state is dropped entirely.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);

        if let Some(s) = self.state.as_mut() {
            if s.open {
                s.open = false;
                cx.notify();
            }
        }

        let entity = cx.entity().downgrade();
        let dismiss_gen = self.generation;
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(TOOLTIP_DISMISS_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                if this.generation == dismiss_gen {
                    this.state = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn is_active(&self) -> bool {
        self.state.is_some()
    }
}

impl Render for TooltipController {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(state) = self.state.clone() else {
            return div().into_any_element();
        };
        render_tooltip(state).into_any_element()
    }
}

fn render_tooltip(state: TooltipState) -> impl IntoElement {
    let open = state.open;
    let text = state.text.clone();
    let pos = state.position;

    let tooltip_id = ElementId::Name("crabport-tooltip".into());

    div()
        .id(tooltip_id.clone())
        .absolute()
        .top(pos.y + px(20.0))
        .left(pos.x)
        .when(open, |el| el.occlude())
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded(RADIUS_SM)
        .shadow_sm()
        .px_2()
        .py_1()
        .text_xs()
        .text_color(rgb(text_primary()))
        .whitespace_nowrap()
        // Pre-set opacity 0 so the transition registry has a concrete
        // value to interpolate *from* on fade-in.
        .opacity(0.0)
        .with_transition(tooltip_id)
        .transition_when_else(
            open,
            DURATION_BASE,
            EASE_STANDARD,
            |el| el.opacity(1.0),
            |el| el.opacity(0.0),
        )
        .child(text)
}
