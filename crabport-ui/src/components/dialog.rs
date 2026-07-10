//! # AlertDialog
//!
//! A reusable modal dialog for confirmation prompts of varying severity
//! (info / warning / danger). Renders a dimmed overlay with a centered
//! dialog card that eases in via opacity + vertical translate, matching
//! the existing overlay animation pattern used by the connection form.
//!
//! The dialog is driven by an [`AlertState`] value. Callers typically
//! hold the state in their own view and toggle `open` to show/hide the
//! dialog; the transitions are handled by `gpui-animation`.
//!
//! ## Usage
//!
//! ```ignore
//! // In your view state:
//! pub alert: AlertState;
//!
//! // To show:
//! self.alert = AlertState {
//!     open: true,
//!     severity: AlertSeverity::Warning,
//!     title: "Unknown Host Key".into(),
//!     description: Some("The authenticity of host example.com can't be established.".into()),
//!     details: vec![
//!         ("Algorithm".into(), "ssh-ed25519".into()),
//!         ("Fingerprint".into(), "SHA256:...".into()),
//!     ],
//!     confirm_label: "Trust & Connect".into(),
//!     cancel_label: "Cancel".into(),
//!     on_confirm: Some(Rc::new(|w, cx| { /* ... */ })),
//!     on_cancel: Some(Rc::new(|w, cx| { /* ... */ })),
//! };
//!
//! // In render:
//! el.child(AlertDialog::new("my-alert", self.alert.clone()))
//! ```

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};

use crate::color::*;
use crate::components::button::Button;
use crate::components::overlay::render_overlay;

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// Visual severity of the alert. Drives the icon color and the confirm
/// button styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlertSeverity {
    /// Neutral informational prompt (blue accent).
    #[default]
    Info,
    /// Cautionary prompt — something the user should think about (yellow).
    Warning,
    /// Destructive / unsafe action (red).
    Danger,
}

impl AlertSeverity {
    /// Accent color used for the icon and (for warning/danger) the confirm
    /// button border highlight.
    fn accent(self) -> u32 {
        match self {
            Self::Info => term_blue(),
            Self::Warning => term_yellow(),
            Self::Danger => term_red(),
        }
    }

    /// Icon path for the leading icon.
    fn icon(self) -> &'static str {
        match self {
            Self::Info | Self::Warning | Self::Danger => "icons/circle-alert.svg",
        }
    }
}

// ---------------------------------------------------------------------------
// AlertState
// ---------------------------------------------------------------------------

/// Immutable snapshot describing one alert prompt. Cloning is cheap (the
/// callbacks are `Rc`).
#[derive(Clone)]
pub struct AlertState {
    /// Whether the dialog is currently shown. Toggling this drives the
    /// overlay + dialog transitions.
    pub open: bool,
    pub severity: AlertSeverity,
    pub title: SharedString,
    /// Optional body text shown under the title.
    pub description: Option<SharedString>,
    /// Optional key/value rows rendered in a muted panel (e.g. algorithm +
    /// fingerprint for host-key prompts).
    pub details: Vec<(SharedString, SharedString)>,
    pub confirm_label: SharedString,
    pub cancel_label: SharedString,
    /// Invoked when the user clicks the confirm button. The dialog is NOT
    /// auto-closed — the caller is responsible for setting `open = false`
    /// (usually inside the callback) so the dismiss animation can run.
    pub on_confirm: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// Invoked when the user clicks cancel or the backdrop. Same closing
    /// contract as `on_confirm`.
    pub on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            open: false,
            severity: AlertSeverity::Info,
            title: SharedString::default(),
            description: None,
            details: Vec::new(),
            confirm_label: SharedString::default(),
            cancel_label: SharedString::default(),
            on_confirm: None,
            on_cancel: None,
        }
    }
}

// ---------------------------------------------------------------------------
// AlertController — global singleton-style alert dialog host
// ---------------------------------------------------------------------------
//
// `AlertController` is an `Entity` meant to be held by the app root and
// rendered as a top-level child (alongside the command palette). It owns at
// most one in-flight `AlertState`.
//
// Showing an alert:
//
// ```ignore
// alert_controller.update(cx, |c, cx| {
//     c.show(AlertState { /* ... */ }, cx);
// });
// ```
//
// The controller wraps the caller's `on_confirm` / `on_cancel` so that the
// dialog always plays its dismiss animation (opacity + translate) rather
// than vanishing instantly. After the animation completes the state is
// cleared, ready for the next prompt.

/// How long the dismiss animation runs before the state is dropped. Should
/// match the `transition_when_else` duration used in `render_dialog`.
const ALERT_DISMISS_MS: u64 = 160;

pub struct AlertController {
    /// `None` when no alert is showing or dismissing.
    state: Option<AlertState>,
}

impl AlertController {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Show an alert dialog immediately. Any currently-showing alert is
    /// replaced (its callbacks are dropped without being invoked — callers
    /// should avoid stacking alerts).
    ///
    /// The `open` flag on `state` is forced to `true` here so callers don't
    /// have to remember to set it. The user-supplied `on_confirm` /
    /// `on_cancel` are wrapped so the dialog animates out on dismiss.
    pub fn show(&mut self, mut state: AlertState, cx: &mut Context<Self>) {
        let user_confirm = state.on_confirm.take();
        let user_cancel = state.on_cancel.take();

        // Wrap each callback so that after invoking the user's handler we
        // flip `open` to false (which drives the dismiss transition) and
        // schedule the state to be cleared once the animation finishes.
        let entity = cx.entity().downgrade();
        state.on_confirm = Some(Rc::new(move |w, cx| {
            if let Some(cb) = user_confirm.as_ref() {
                cb(w, cx);
            }
            let _ = entity.update(cx, |this, cx| this.begin_dismiss(cx));
        }));

        let entity = cx.entity().downgrade();
        state.on_cancel = Some(Rc::new(move |w, cx| {
            if let Some(cb) = user_cancel.as_ref() {
                cb(w, cx);
            }
            let _ = entity.update(cx, |this, cx| this.begin_dismiss(cx));
        }));

        state.open = true;
        self.state = Some(state);
        cx.notify();
    }

    /// Begin the dismiss animation. Called from the wrapped confirm/cancel
    /// callbacks (and could be called directly to programmatically close).
    pub fn begin_dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.state.as_mut() {
            if s.open {
                s.open = false;
                cx.notify();
            }
        }

        // Schedule cleanup after the dismiss animation completes. We use a
        // smol timer (the crate is already a dependency and used elsewhere
        // in the terminal view) so this works regardless of which runtime
        // is driving the app.
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(ALERT_DISMISS_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                this.state = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Returns `true` when an alert is currently visible (showing or
    /// dismissing). Useful for callers that want to avoid opening a second
    /// alert on top of an existing one.
    pub fn is_active(&self) -> bool {
        self.state.is_some()
    }
}

impl Render for AlertController {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(state) = self.state.clone() else {
            // Render nothing when no alert is active. A plain `div()` keeps
            // the return type stable across renders.
            return div().into_any_element();
        };
        AlertDialog::new("global-alert", state).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// AlertDialog
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct AlertDialog {
    id: ElementId,
    state: AlertState,
}

impl AlertDialog {
    /// Create a new alert dialog.
    ///
    /// `id` must be stable across renders for the same logical dialog so
    /// that `gpui-animation` can track its transition state. It also needs
    /// to be unique among simultaneously-shown dialogs.
    pub fn new(id: impl Into<ElementId>, state: AlertState) -> Self {
        Self {
            id: id.into(),
            state,
        }
    }
}

impl RenderOnce for AlertDialog {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let open = self.state.open;
        let on_cancel = self.state.on_cancel.clone();
        let on_confirm = self.state.on_confirm.clone();

        // Derive stable transition ids from the caller-supplied id. They must
        // be stable across renders (so gpui-animation can track state) and
        // unique among simultaneously-shown dialogs (the caller's
        // responsibility — pass distinct `id`s).
        let overlay_id = self.id_suffix("-alert-overlay");
        let dialog_id = self.id_suffix("-alert-dialog");

        let dialog = render_dialog(
            dialog_id,
            open,
            self.state.severity,
            self.state.title.clone(),
            self.state.description.clone(),
            self.state.details.clone(),
            self.state.confirm_label.clone(),
            self.state.cancel_label.clone(),
            on_confirm,
            on_cancel.clone(),
        );

        render_overlay(overlay_id, open, on_cancel, dialog)
    }
}

impl AlertDialog {
    /// Build a transition id by appending a suffix to the caller-supplied id.
    /// Falls back to a `CodeLocation`-style placeholder for non-Name ids
    /// (which shouldn't happen in practice — callers always pass `Name`).
    fn id_suffix(&self, suffix: &str) -> ElementId {
        match &self.id {
            ElementId::Name(n) => {
                let mut s = n.to_string();
                s.push_str(suffix);
                ElementId::Name(s.into())
            }
            _ => self.id.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_dialog(
    dialog_id: ElementId,
    open: bool,
    severity: AlertSeverity,
    title: SharedString,
    description: Option<SharedString>,
    details: Vec<(SharedString, SharedString)>,
    confirm_label: SharedString,
    cancel_label: SharedString,
    on_confirm: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let accent = severity.accent();
    let has_details = !details.is_empty();

    div()
        .id(dialog_id.clone())
        .w(px(420.0))
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded_lg()
        .shadow_lg()
        .flex()
        .flex_col()
        .p_6()
        .gap_4()
        // Initial hidden state; the transition animates these to visible.
        .opacity(0.0)
        .mt(px(-16.0))
        // Prevent clicks on the dialog from bubbling to the overlay (which
        // would dismiss it).
        .when(open, |el| {
            el.on_click(|_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            open,
            Duration::from_millis(150),
            Linear,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // Title row: icon + heading
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    svg()
                        .path(severity.icon())
                        .size(px(20.0))
                        .flex_shrink_0()
                        .text_color(rgb(accent)),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(text_primary()))
                        .child(title.to_string()),
                ),
        )
        // Optional description
        .when_some(description, |el, desc| {
            el.child(
                div()
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .child(desc.to_string()),
            )
        })
        // Optional key/value details panel
        .when(has_details, |el| {
            el.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded(px(6.0))
                    .bg(rgb(bg_sidebar()))
                    .children(
                        details
                            .iter()
                            .enumerate()
                            .map(|(i, (k, v))| render_detail_row(i, k, v)),
                    ),
            )
        })
        // Action buttons
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child({
                    let mut btn = Button::new(ElementId::Name("alert-cancel".into()))
                        .centered(true)
                        .child(cancel_label.to_string());
                    if let Some(cb) = on_cancel {
                        btn = btn.on_click(move |_, w, a| cb(w, a));
                    }
                    btn
                })
                .child({
                    let mut btn = Button::new(ElementId::Name("alert-confirm".into()))
                        .centered(true)
                        .primary()
                        .child(confirm_label.to_string());
                    if let Some(cb) = on_confirm {
                        btn = btn.on_click(move |_, w, a| cb(w, a));
                    }
                    btn
                }),
        )
}

fn render_detail_row(idx: usize, label: &SharedString, value: &SharedString) -> impl IntoElement {
    div()
        .id(ElementId::Name(format!("alert-detail-{}", idx).into()))
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                // Allow long values (e.g. base64 fingerprints) to wrap so
                // they don't blow out the dialog width.
                .text_xs()
                .whitespace_normal()
                .text_color(rgb(text_primary()))
                .child(value.to_string()),
        )
}
