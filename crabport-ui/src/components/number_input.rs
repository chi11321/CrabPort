//! # StyledNumberInput
//!
//! A design-system-native number input that wraps `gpui_component::input::Input`,
//! adding `−` / `+` stepper buttons on the left and right. Only digits are
//! accepted; the caller wires up an [`InputEvent::Change`] subscription via
//! [`subscribe_number_filter`] to enforce the numeric-only constraint and to
//! receive the parsed integer value.
//!
//! ## Visual states
//!
//! Mirrors [`crate::components::input::StyledInput`]:
//!
//! ```text
//!  rest     ── input_bg() bg · input_border() border
//!  hover    ── input_border_hover() border           (120 ms Linear)
//!  focus    ── input_bg_focused() bg · input_border_focused() border
//!  error    ── input_border_error() border            (hover suppressed)
//!  disabled ── input_bg_disabled() bg, muted text, no pointer events
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! // In your View:
//! //   field: Entity<InputState>,  // created via cx.new(|cx| InputState::new(window, cx))
//! //   field_focused: bool,
//! //
//! // At construction (where you have a &mut Window), wire the numeric filter:
//! //   StyledNumberInput::subscribe_number_filter(&field, 8, 32, 1, window, cx, |this, value, cx| {
//! //       // persist `value` ...
//! //       cx.notify();
//! //   });
//!
//! StyledNumberInput::new("font-size", self.field.clone())
//!     .label("Font size")
//!     .focused(self.field_focused)
//!     .min(8)
//!     .max(32)
//!     .step(1)
//! ```

use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::{Input, InputEvent, InputState};
use std::time::Duration;

use crate::color::*;

// ---------------------------------------------------------------------------
// StyledNumberInput
// ---------------------------------------------------------------------------

/// A numeric stepper input: `−` button · text field · `+` button, sharing
/// one rounded shell with the same focus/hover/error styling as
/// [`StyledInput`](crate::components::input::StyledInput).
///
/// The `−` / `+` buttons adjust the value by [`Self::step`] and clamp into
/// `[min, max]`. Typing is also allowed; the caller enforces digits-only via
/// [`subscribe_number_filter`] (the component itself can't hold a subscription
/// because it's a transient `RenderOnce` element rebuilt every frame).
#[derive(IntoElement)]
pub struct StyledNumberInput {
    id: SharedString,
    state: Entity<InputState>,
    label: Option<SharedString>,
    error: Option<SharedString>,
    /// Whether the text field currently has keyboard focus (drives accent
    /// border). Tracked by the owner via `InputState`'s on_focus/on_blur.
    focused: bool,
    disabled: bool,
    height: Pixels,
    min: i64,
    max: i64,
    step: i64,
}

impl StyledNumberInput {
    pub fn new(id: impl Into<SharedString>, state: Entity<InputState>) -> Self {
        Self {
            id: id.into(),
            state,
            label: None,
            error: None,
            focused: false,
            disabled: false,
            height: px(32.0),
            min: 0,
            max: i64::MAX,
            step: 1,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Pass `true` when the field has keyboard focus (drives accent border).
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Puts the field into error state and shows `msg` below it.
    pub fn error(mut self, msg: impl Into<SharedString>) -> Self {
        self.error = Some(msg.into());
        self
    }

    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }

    /// Override the shell height (default `px(32.0)`).
    pub fn height(mut self, h: Pixels) -> Self {
        self.height = h;
        self
    }

    /// Inclusive lower bound for the stepper buttons. Defaults to `0`.
    pub fn min(mut self, min: i64) -> Self {
        self.min = min;
        self
    }

    /// Inclusive upper bound for the stepper buttons. Defaults to `i64::MAX`.
    pub fn max(mut self, max: i64) -> Self {
        self.max = max;
        self
    }

    /// Amount added/subtracted by the `−` / `+` buttons. Defaults to `1`.
    pub fn step(mut self, step: i64) -> Self {
        self.step = step;
        self
    }
}

impl RenderOnce for StyledNumberInput {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let has_error = self.error.is_some();
        let focused = self.focused;
        let height = self.height;
        let disabled = self.disabled;
        let min = self.min;
        let max = self.max;
        let step = self.step;

        // Background priority: disabled > focus > rest.
        let base_bg: u32 = if disabled {
            input_bg_disabled()
        } else if focused {
            input_bg_focused()
        } else {
            input_bg()
        };

        // Border priority: error > focus > rest.
        let base_border: u32 = if has_error {
            input_border_error()
        } else if focused {
            input_border_focused()
        } else {
            input_border()
        };

        let col_id = ElementId::Name(format!("{}-col", self.id).into());
        let shell_id = ElementId::Name(format!("{}-shell", self.id).into());

        let state = self.state.clone();

        // The `−` / `+` stepper buttons are plain interactive divs that sit
        // flush against the input inside the shared shell. They use a flat
        // `btn_bg()` background (no hover state, no rounded corners — the
        // shell's `overflow_hidden` + `rounded_md` clips their outer edges).
        let minus_state = state.clone();
        let minus_btn = div()
            .id(ElementId::Name(format!("{}-minus", self.id).into()))
            .flex()
            .items_center()
            .justify_center()
            .h(height)
            .w(height)
            .flex_shrink_0()
            .bg(rgb(btn_bg()))
            .when(disabled, |el| el.cursor_not_allowed().opacity(0.5))
            .child(
                svg()
                    .path("icons/minus.svg")
                    .size_3p5()
                    .text_color(rgb(text_muted())),
            )
            .on_click(move |_, window, cx| {
                if disabled {
                    return;
                }
                let cur = parse_input_value(&minus_state, cx);
                let next = (cur - step).clamp(min, max);
                if next != cur {
                    minus_state.update(cx, |s, cx| {
                        s.set_value(next.to_string(), window, cx);
                    });
                }
            });

        // The `+` button.
        let plus_state = state.clone();
        let plus_btn = div()
            .id(ElementId::Name(format!("{}-plus", self.id).into()))
            .flex()
            .items_center()
            .justify_center()
            .h(height)
            .w(height)
            .flex_shrink_0()
            .bg(rgb(btn_bg()))
            .when(disabled, |el| el.cursor_not_allowed().opacity(0.5))
            .child(
                svg()
                    .path("icons/plus.svg")
                    .size_3p5()
                    .text_color(rgb(text_muted())),
            )
            .on_click(move |_, window, cx| {
                if disabled {
                    return;
                }
                let cur = parse_input_value(&plus_state, cx);
                let next = (cur + step).clamp(min, max);
                if next != cur {
                    plus_state.update(cx, |s, cx| {
                        s.set_value(next.to_string(), window, cx);
                    });
                }
            });

        let shell = div()
            .id(shell_id.clone())
            .flex()
            .flex_row()
            .items_center()
            .h(height)
            .w_full()
            .overflow_hidden()
            .rounded_md()
            .bg(rgb(base_bg))
            .border_1()
            .border_color(rgb(base_border))
            .with_transition(shell_id)
            .transition_on_hover(Duration::from_millis(120), Linear, move |hovered, el| {
                if has_error || focused {
                    el
                } else if *hovered {
                    el.border_color(rgb(input_border_hover()))
                } else {
                    el.border_color(rgb(input_border()))
                }
            })
            .child(minus_btn)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        Input::new(&state)
                            .appearance(false)
                            .bordered(false)
                            .disabled(disabled)
                            .text_sm()
                            .text_center(),
                    ),
            )
            .child(plus_btn);

        div()
            .id(col_id)
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .when(disabled, |el| el.cursor_not_allowed().opacity(0.5))
            .when_some(self.label, |el, label| {
                el.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(text_muted()))
                        .child(label),
                )
            })
            .child(shell)
            .when_some(self.error, |el, msg| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            svg()
                                .path("icons/circle-alert.svg")
                                .size_3()
                                .text_color(rgb(input_border_error())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(input_border_error()))
                                .child(msg),
                        ),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// Numeric filter subscription helper
// ---------------------------------------------------------------------------

/// Subscribe to [`InputEvent::Change`] on a numeric `InputState` and enforce a
/// digits-only constraint with `[min, max]` clamping.
///
/// On every change:
/// 1. The raw text is stripped of non-digit characters.
/// 2. The remaining digits are parsed to an integer and clamped into `[min, max]`.
/// 3. If the cleaned value differs from what the user typed, the input is
///    reset to the cleaned value (so `12abc3` becomes `12` then `123`-ish —
///    only valid integers ever stay on screen).
/// 4. `on_change` is invoked with the final clamped integer so the owner can
///    persist it.
///
/// This must be called from a context that has a `&mut Window` (e.g. the
/// view's constructor or an `on_focus`/`on_blur` handler), because resetting
/// the value requires `set_value(.., window, cx)`. The `on_change` callback
/// runs inside a deferred `entity.update` so it receives a proper
/// `&mut Context<T>`.
pub fn subscribe_number_filter<T: 'static>(
    state: &Entity<InputState>,
    min: i64,
    max: i64,
    window: &mut Window,
    cx: &mut Context<T>,
    mut on_change: impl FnMut(&mut T, i64, &mut Context<T>) + 'static,
) -> Subscription {
    let state_c = state.clone();
    let entity = cx.entity().downgrade();
    window.subscribe(
        state,
        cx,
        move |_entity, _event: &InputEvent, window, cx| {
            let raw = state_c.read(cx).value().to_string();
            let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
            let parsed: i64 = digits.parse().unwrap_or(min);
            let clamped = parsed.clamp(min, max);
            // Only rewrite when the cleaned representation differs from what
            // the user typed — otherwise we'd fight the caret on every keystroke
            // for valid input (e.g. typing "1" then "3" would re-set twice).
            if clamped.to_string() != raw {
                state_c.update(cx, |s, cx| {
                    s.set_value(clamped.to_string(), window, cx);
                });
            }
            // Deliver the canonical value to the owning view. `window.subscribe`
            // gives us `&mut App`, so we upgrade the view's weak handle and
            // run `on_change` inside `entity.update` where a `&mut Context<T>`
            // is available.
            let value = clamped;
            let _ = entity.update(cx, |this, cx| {
                on_change(this, value, cx);
            });
        },
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the current integer value of an `InputState`. Non-digit characters
/// are stripped first; an empty/all-invalid string yields `0` (the caller's
/// `min` clamp then takes over).
fn parse_input_value(state: &Entity<InputState>, cx: &App) -> i64 {
    let raw = state.read(cx).value().to_string();
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}
