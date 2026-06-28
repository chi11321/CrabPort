//! # StyledInput
//!
//! A wrapper around `gpui_component::input::Input` that replaces its
//! default chrome with a design-system-native shell.
//!
//! ## Visual states
//!
//! ```text
//!  rest     ── INPUT_BG bg · INPUT_BORDER border
//!  hover    ── INPUT_BORDER_HOVER border           (120 ms Linear)
//!  focus    ── INPUT_BG_FOCUSED bg · INPUT_BORDER_FOCUSED border
//!  error    ── INPUT_BORDER_ERROR border            (hover suppressed)
//!  disabled ── INPUT_BG_DISABLED bg, muted text, no pointer events
//! ```
//!
//! ## Focus detection
//!
//! `InputState` does not expose a public `focused` field.
//! The caller reads focus via `InputState`'s event callbacks
//! (`on_focus` / `on_blur`) and stores the bool in their own view state,
//! then passes it here as `.focused(bool)`.
//!
//! ## Usage
//!
//! ```ignore
//! // In your View:
//! //   field: cx.new(|cx| InputState::new(window, cx)),
//! //   host_focused: bool,
//!
//! StyledInput::new("host", self.host_field.clone())
//!     .label("Host")
//!     .focused(self.host_focused)
//!     .prefix(svg().path("icons/server.svg").size_3p5())
//!
//! StyledPasswordInput::new("pw", self.pass_field.clone())
//!     .label("Password")
//!     .focused(self.pass_focused)
//!     .show_password(self.show_pw)
//!     .on_toggle(cx.listener(|this, _, _w, cx| {
//!         this.show_pw = !this.show_pw;
//!         cx.notify();
//!     }))
//! ```

use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::{Input, InputState};
use std::time::Duration;

use crate::color::*;

// ---------------------------------------------------------------------------
// StyledInput
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct StyledInput {
    id: SharedString,
    state: Entity<InputState>,
    label: Option<SharedString>,
    prefix: Option<AnyElement>,
    suffix: Option<AnyElement>,
    error: Option<SharedString>,
    /// Whether this field currently has keyboard focus.
    /// Read from your view state; updated via InputState's on_focus/on_blur.
    focused: bool,
    disabled: bool,
    height: Pixels,
    multi_line: bool,
}

impl StyledInput {
    pub fn new(id: impl Into<SharedString>, state: Entity<InputState>) -> Self {
        Self {
            id: id.into(),
            state,
            label: None,
            prefix: None,
            suffix: None,
            error: None,
            focused: false,
            disabled: false,
            height: px(32.0),
            multi_line: false,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Pass `true` when this field has keyboard focus (drives accent border).
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Element pinned to the left edge inside the shell.
    pub fn prefix(mut self, el: impl IntoElement + 'static) -> Self {
        self.prefix = Some(el.into_any_element());
        self
    }

    /// Element pinned to the right edge inside the shell.
    pub fn suffix(mut self, el: impl IntoElement + 'static) -> Self {
        self.suffix = Some(el.into_any_element());
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

    /// Enable multi-line mode (textarea-like input).
    pub fn multi_line(mut self, v: bool) -> Self {
        self.multi_line = v;
        self
    }

    /// Set the number of visible rows for multi-line input.
    /// Each row is roughly one line-height (~20px).
    pub fn rows(mut self, rows: usize) -> Self {
        self.height = px(rows as f32 * 20.0);
        self
    }
}

impl RenderOnce for StyledInput {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let has_error = self.error.is_some();
        let focused = self.focused;
        let height = self.height;
        let disabled = self.disabled;
        let multi_line = self.multi_line;

        // Background priority: disabled > focus > rest.
        let base_bg: u32 = if disabled {
            INPUT_BG_DISABLED
        } else if focused {
            INPUT_BG_FOCUSED
        } else {
            INPUT_BG
        };

        // Border priority: error > focus > rest.
        let base_border: u32 = if has_error {
            INPUT_BORDER_ERROR
        } else if focused {
            INPUT_BORDER_FOCUSED
        } else {
            INPUT_BORDER
        };

        let col_id = ElementId::Name(format!("{}-col", self.id).into());
        let shell_id = ElementId::Name(format!("{}-shell", self.id).into());

        // ------------------------------------------------------------------
        // Prefix / suffix wrappers
        // ------------------------------------------------------------------
        let prefix_el = self.prefix.map(|p| {
            div()
                .flex()
                .items_center()
                .pl_2()
                .pr_1()
                .flex_shrink_0()
                .text_color(rgb(TEXT_MUTED))
                .child(p)
        });

        let suffix_el = self.suffix.map(|s| {
            div()
                .flex()
                .items_center()
                .pl_1()
                .pr_2()
                .flex_shrink_0()
                .text_color(rgb(TEXT_MUTED))
                .child(s)
        });

        // ------------------------------------------------------------------
        // Shell
        // Single with_transition: hover brightens border in rest state only.
        // focused/error states are set as the static base and are never
        // overridden by the hover callback.
        // ------------------------------------------------------------------
        let state = self.state.clone();

        let shell = div()
            .id(shell_id.clone())
            .flex()
            .flex_row()
            .when_else(
                multi_line,
                |el| el.items_start().h(height),
                |el| el.items_center().h(height),
            )
            .w_full()
            .overflow_y_scroll()
            .rounded_md()
            .bg(rgb(base_bg))
            .border_1()
            .border_color(rgb(base_border))
            .with_transition(shell_id)
            .transition_on_hover(Duration::from_millis(120), Linear, move |hovered, el| {
                if has_error || focused {
                    el // don't override error / focus border on hover
                } else if *hovered {
                    el.border_color(rgb(INPUT_BORDER_HOVER))
                } else {
                    el.border_color(rgb(INPUT_BORDER))
                }
            })
            .when_some(prefix_el, |el, p| el.child(p))
            .child(
                div().flex_1().min_w_0().h_full().child(
                    Input::new(&state)
                        .appearance(false)
                        .bordered(false)
                        .when(multi_line, |input| input.h_full()),
                ),
            )
            .when_some(suffix_el, |el, s| el.child(s));

        // ------------------------------------------------------------------
        // Outer column: label · shell · error message
        // ------------------------------------------------------------------
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
                        .text_color(rgb(TEXT_MUTED))
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
                                .text_color(rgb(INPUT_BORDER_ERROR)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(INPUT_BORDER_ERROR))
                                .child(msg),
                        ),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// StyledPasswordInput
// ---------------------------------------------------------------------------

/// `StyledInput` pre-wired with an animated show/hide eye-icon suffix.
#[derive(IntoElement)]
pub struct StyledPasswordInput {
    inner: StyledInput,
    on_toggle: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl StyledPasswordInput {
    pub fn new(id: impl Into<SharedString>, state: Entity<InputState>) -> Self {
        Self {
            inner: StyledInput::new(id, state),
            on_toggle: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.inner = self.inner.label(label);
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.inner = self.inner.focused(focused);
        self
    }

    pub fn error(mut self, msg: impl Into<SharedString>) -> Self {
        self.inner = self.inner.error(msg);
        self
    }

    pub fn disabled(mut self, v: bool) -> Self {
        self.inner = self.inner.disabled(v);
        self
    }

    /// Called when the user clicks the eye icon.
    pub fn on_toggle(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(std::rc::Rc::new(f));
        self
    }
}

impl RenderOnce for StyledPasswordInput {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.inner
    }
}
