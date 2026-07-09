//! # Switch
//!
//! A toggle switch component with an animated sliding knob. Visual states:
//!
//! ```text
//!  on       ── btn_primary_bg() track · white knob on the right
//!  off      ── surface_active() track · text_muted() knob on the left
//!  disabled ── muted track, no pointer events
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! Switch::new("my-switch")
//!     .checked(self.flag)
//!     .on_change(cx.listener(|this, v, _w, cx| {
//!         this.flag = *v;
//!         cx.notify();
//!     }))
//! ```
//!
//! The `on_change` callback fires with the *new* (toggled) value, so the
//! caller does not need to invert the bool themselves.

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_animation::transition::general::EaseInOutQuad;

use crate::color::*;

/// Dimensions for the switch track and knob. Tuned to match the height of
/// the `StyledInput` shell (`px(32.0)`) so a switch aligns with a labelled
/// input row.
const TRACK_W: Pixels = px(34.0);
const TRACK_H: Pixels = px(20.0);
const KNOB_SIZE: Pixels = px(16.0);
const KNOB_MARGIN: Pixels = px(2.0);

/// Animated toggle switch.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            on_change: None,
        }
    }

    /// Current on/off state. Drives the knob position and track color.
    pub fn checked(mut self, v: bool) -> Self {
        self.checked = v;
        self
    }

    /// Disable interaction and visually mute the switch.
    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }

    /// Called with the *new* value when the user toggles the switch.
    pub fn on_change(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// Clean up gpui-animation state associated with this Switch.
    /// Call this when the component is removed from the render tree.
    pub fn cleanup_animation(id: &ElementId) {
        let track_id = ElementId::Name(format!("{id:?}-track").into());
        let knob_id = ElementId::Name(format!("{id:?}-knob").into());
        gpui_animation::reset_transition(&track_id);
        gpui_animation::reset_transition(&knob_id);
    }
}

impl RenderOnce for Switch {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let checked = self.checked;
        let disabled = self.disabled;
        let on_change = self.on_change.clone();

        // Track background: primary accent when on, muted surface when off.
        // Disabled overrides both to a desaturated tone.
        let track_bg_on = btn_primary_bg();
        let track_bg_off = surface_active();
        let track_bg_disabled = input_bg_disabled();

        let track_id = ElementId::Name(format!("{:?}-track", self.id).into());
        let knob_id = ElementId::Name(format!("{:?}-knob", self.id).into());

        // Knob horizontal offset. Off = left margin; on = right edge.
        // Computed from track width so the knob never overshoots.
        let off_left = KNOB_MARGIN;
        let on_left = TRACK_W - KNOB_SIZE - KNOB_MARGIN;

        // Knob color: white when on (sits on the accent), muted when off.
        let knob_on = 0xffffff;
        let knob_off = text_muted();
        let knob_disabled = btn_text_disabled();

        // ------------------------------------------------------------------
        // Knob — absolute, slides between left and right.
        //
        // Both `left` and `bg` are driven by `transition_when_else` so the
        // knob re-colors in lockstep with its slide when `checked` changes.
        // Computing `bg` as a static value before the transition (the
        // previous implementation) made gpui-animation cache the initial
        // color and ignore subsequent `checked` changes.
        // ------------------------------------------------------------------
        let knob = div()
            .id(knob_id.clone())
            .absolute()
            .top(KNOB_MARGIN)
            .h(KNOB_SIZE)
            .w(KNOB_SIZE)
            .rounded_full()
            .bg(rgb(if disabled { knob_disabled } else { knob_off }))
            .with_transition(knob_id)
            .transition_when_else(
                checked && !disabled,
                Duration::from_millis(180),
                EaseInOutQuad,
                move |s| s.left(on_left).bg(rgb(knob_on)),
                move |s| s.left(off_left).bg(rgb(knob_off)),
            );

        // ------------------------------------------------------------------
        // Track — relative, holds the knob. Background color also animates.
        // ------------------------------------------------------------------
        let track = div()
            .id(track_id.clone())
            .relative()
            .h(TRACK_H)
            .w(TRACK_W)
            .flex_none()
            .rounded_full()
            .bg(rgb(if disabled {
                track_bg_disabled
            } else {
                track_bg_off
            }))
            .with_transition(track_id)
            .transition_when_else(
                checked && !disabled,
                Duration::from_millis(180),
                EaseInOutQuad,
                move |s| s.bg(rgb(track_bg_on)),
                move |s| s.bg(rgb(track_bg_off)),
            )
            .child(knob);

        // ------------------------------------------------------------------
        // Root — wraps the track so the click target is a comfortable size.
        // ------------------------------------------------------------------
        div()
            .id(self.id)
            .flex()
            .items_center()
            .h(TRACK_H)
            .when_else(
                disabled,
                |el| el.cursor_not_allowed().opacity(0.6),
                |el| {
                    el.when_some(on_change, |el, cb| {
                        el.on_click(move |_e, w, cx| {
                            // Toggle and dispatch the *new* value.
                            cb(&!checked, w, cx);
                            cx.stop_propagation();
                        })
                    })
                },
            )
            .child(track)
    }
}
