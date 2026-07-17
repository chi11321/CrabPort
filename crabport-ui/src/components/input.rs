//! # StyledInput
//!
//! A wrapper around `gpui_component::input::Input` that replaces its
//! default chrome with a design-system-native shell.
//!
//! ## Visual states
//!
//! ```text
//!  rest     ── input_bg() bg · input_border() border
//!  hover    ── input_border_hover() border           (120 ms Linear)
//!  focus    ── input_bg_focused() bg · input_border_focused() border
//!  error    ── input_border_error() border            (hover suppressed)
//!  disabled ── input_bg_disabled() bg, muted text, no pointer events
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
use gpui_component::{Sizable, Size};
use std::time::Duration;

use crate::color::*;

// ---------------------------------------------------------------------------
// Shared label + shell + error column builder
// ---------------------------------------------------------------------------

/// Build the outer column (label · shell · error message) shared by
/// [`StyledInput`] and [`StyledNumberInput`]. Both render the same
/// structure around their inner shell: an optional label above, the shell,
/// and an optional error row below with a leading alert icon.
pub(crate) fn labeled_error_column(
    col_id: impl Into<ElementId>,
    disabled: bool,
    label: Option<SharedString>,
    shell: impl IntoElement,
    error: Option<SharedString>,
) -> impl IntoElement {
    div()
        .id(col_id.into())
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .when(disabled, |el| el.cursor_not_allowed().opacity(0.5))
        .when_some(label, |el, label| {
            el.child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(text_muted()))
                    .child(label),
            )
        })
        .child(shell)
        .when_some(error, |el, msg| {
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
    /// When `true`, the underlying `Input` widget refuses keyboard edits but
    /// the shell keeps its normal (non-dimmed) appearance. Used by read-only
    /// fields that are filled programmatically (e.g. the private-key file
    /// path, which is set only via the Browse button) so an interactive
    /// suffix button inside the shell doesn't look disabled.
    input_disabled: bool,
    height: Pixels,
    multi_line: bool,
    /// Optional override for the input text size (default inherits from
    /// `gpui-component` Input, which is `text_sm` for `Size::Medium`).
    input_size: Option<Size>,
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
            input_disabled: false,
            height: px(32.0),
            multi_line: false,
            input_size: None,
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

    /// Disable keyboard editing of the underlying `Input` widget but leave
    /// the shell visually enabled. See [`Self::input_disabled`] field docs.
    /// Implies the same effect as `disabled(true)` on the `Input` component
    /// without the `opacity(0.5)` / `cursor_not_allowed()` dimming.
    pub fn input_disabled(mut self, v: bool) -> Self {
        self.input_disabled = v;
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

    /// Override the input text size. Useful for compact inputs in side
    /// panels (e.g. SFTP path bar).
    pub fn text_size(mut self, size: Pixels) -> Self {
        // `Size::Size(s)` makes gpui-component render text at `s * 0.875`.
        // Solve for the pixel value that yields the requested size.
        self.input_size = Some(Size::Size(size * (1.0 / 0.875)));
        self
    }

    /// Compact variant: smaller text + shorter height. Convenience for
    /// `text_size(px(11.0)).height(px(26.0))`.
    pub fn xsmall(mut self) -> Self {
        self.input_size = Some(Size::XSmall);
        self.height = px(26.0);
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
        // The underlying Input widget is disabled when either flag is set —
        // `disabled` dims the whole shell, `input_disabled` only blocks edits.
        let input_disabled = self.disabled || self.input_disabled;
        let multi_line = self.multi_line;

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
        let input_size = self.input_size;

        // ------------------------------------------------------------------
        // Prefix / suffix wrappers
        // ------------------------------------------------------------------
        // In xsmall mode (`Size::XSmall`) use tighter padding so the icon sits
        // closer to the left edge.
        let (prefix_pl, prefix_pr, suffix_pl, suffix_pr) = match input_size {
            Some(Size::XSmall) => (px(6.0), px(4.0), px(4.0), px(6.0)),
            _ => (px(8.0), px(4.0), px(4.0), px(8.0)),
        };
        let prefix_el = self.prefix.map(|p| {
            div()
                .flex()
                .items_center()
                .pl(prefix_pl)
                .pr(prefix_pr)
                .flex_shrink_0()
                .text_color(rgb(text_muted()))
                .child(p)
        });

        let suffix_el = self.suffix.map(|s| {
            div()
                .flex()
                .items_center()
                .pl(suffix_pl)
                .pr(suffix_pr)
                .flex_shrink_0()
                .text_color(rgb(text_muted()))
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
                    el.border_color(rgb(input_border_hover()))
                } else {
                    el.border_color(rgb(input_border()))
                }
            })
            .when_some(prefix_el, |el, p| el.child(p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .when_else(multi_line, |el| el.items_start(), |el| el.items_center())
                    .child(
                        Input::new(&state)
                            .appearance(false)
                            .bordered(false)
                            .disabled(input_disabled)
                            .when_some(input_size, |input, size| input.with_size(size))
                            .when(multi_line, |input| input.h_full()),
                    ),
            )
            .when_some(suffix_el, |el, s| el.child(s));

        // ------------------------------------------------------------------
        // Outer column: label · shell · error message (shared builder)
        // ------------------------------------------------------------------
        labeled_error_column(col_id, disabled, self.label, shell, self.error)
    }
}

// ---------------------------------------------------------------------------
// StyledPasswordInput
// ---------------------------------------------------------------------------

/// `StyledInput` pre-wired with an animated show/hide eye-icon suffix.
#[derive(IntoElement)]
pub struct StyledPasswordInput {
    inner: StyledInput,
}

impl StyledPasswordInput {
    pub fn new(id: impl Into<SharedString>, state: Entity<InputState>) -> Self {
        Self {
            inner: StyledInput::new(id, state),
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
}

impl RenderOnce for StyledPasswordInput {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.inner
    }
}
