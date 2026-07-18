//! # Notification (toast)
//!
//! Transient, non-modal toast notifications that pop in at the bottom-right
//! or top-center of the window and auto-dismiss after a configurable
//! duration. Like [`ContextMenuController`] / [`AlertController`], the host
//! is an `Entity` held by the app root and rendered as a top-level child.
//!
//! Each notification eases in via opacity + vertical translate, sits on top
//! of the regular UI without blocking interaction (no overlay backdrop), and
//! eases out — either after the auto-dismiss timer fires, when the user
//! clicks the close button, or when a follow-up action is invoked.
//!
//! ## Usage
//!
//! ```ignore
//! notification_controller.update(cx, |c, cx| {
//!     c.show(
//!         Notification::new("Host key saved")
//!             .level(NotificationLevel::Success)
//!             .message("Fingerprint stored to known_hosts.")
//!             .duration(Duration::from_secs(4)),
//!         cx,
//!     );
//! });
//! ```
//!
//! Set a custom position with [`NotificationController::with_position`] or
//! via the constructor argument:
//!
//! ```ignore
//! cx.new(|_| NotificationController::new(NotificationPosition::TopCenter));
//! ```

use std::rc::Rc;
use std::time::Duration;

use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{EASE_OUT, RADIUS_MD, RADIUS_XS};

// ---------------------------------------------------------------------------
// NotificationLevel
// ---------------------------------------------------------------------------

/// Visual level of a notification. Drives the leading icon and accent color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationLevel {
    /// Neutral informational message (blue accent).
    #[default]
    Info,
    /// Positive outcome — something succeeded (green accent).
    Success,
    /// Cautionary notice (yellow accent).
    Warning,
    /// Error / failure (red accent).
    Danger,
}

impl NotificationLevel {
    fn accent(self) -> u32 {
        match self {
            Self::Info => term_blue(),
            Self::Success => term_green(),
            Self::Warning => term_yellow(),
            Self::Danger => term_red(),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Info | Self::Warning => "icons/circle-alert.svg",
            Self::Success => "icons/circle-check.svg",
            Self::Danger => "icons/circle-x.svg",
        }
    }
}

// ---------------------------------------------------------------------------
// NotificationPosition
// ---------------------------------------------------------------------------

/// Where on the window the toast stack is anchored. Notifications stack
/// vertically away from the anchor edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationPosition {
    /// Bottom-right corner (default). Stack grows upward.
    #[default]
    BottomRight,
    /// Top-center. Stack grows downward.
    TopCenter,
}

// ---------------------------------------------------------------------------
// Notification (immutable snapshot)
// ---------------------------------------------------------------------------

/// One toast invocation. Cloning is cheap (callbacks are `Rc`).
#[derive(Clone)]
pub struct Notification {
    /// Stable id used to derive per-notification transition ids. If two
    /// simultaneously-shown toasts share an id their animations will
    /// collide, so use something unique per invocation (a counter works).
    pub id: SharedString,
    pub level: NotificationLevel,
    pub title: SharedString,
    /// Optional body text under the title.
    pub message: Option<SharedString>,
    /// How long the toast stays open before auto-dismissing. Set to
    /// `Duration::ZERO` to disable auto-dismiss (the user must click close).
    pub duration: Duration,
    /// Optional action button rendered at the trailing edge (e.g. "Undo").
    /// Invoking it does NOT auto-dismiss — wrap your callback to call
    /// `controller.dismiss(id)` if you want it to.
    pub action_label: Option<SharedString>,
    pub action: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Notification {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            id: SharedString::default(),
            level: NotificationLevel::Info,
            title: title.into(),
            message: None,
            duration: DEFAULT_DURATION,
            action_label: None,
            action: None,
        }
    }

    /// Override the auto-generated id. Useful if you later want to
    /// programmatically dismiss a specific toast via `controller.dismiss(id)`.
    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = id.into();
        self
    }

    pub fn level(mut self, level: NotificationLevel) -> Self {
        self.level = level;
        self
    }

    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Set the auto-dismiss duration. `Duration::ZERO` disables auto-dismiss.
    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Attach a trailing action button. `label` is the button text; `action`
    /// is invoked when the user clicks it. The toast is NOT auto-dismissed
    /// — call `controller.dismiss(id)` from inside your callback if needed.
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        action: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.action_label = Some(label.into());
        self.action = Some(Rc::new(action));
        self
    }
}

// ---------------------------------------------------------------------------
// Internal entry — the controller's per-toast runtime state
// ---------------------------------------------------------------------------

/// One live (or dismissing) toast. `generation` lets a stale dismiss task
/// bail out if the same slot has been re-shown in the meantime.
#[derive(Clone)]
struct NotificationEntry {
    notification: Notification,
    /// Drives the in/out transition.
    open: bool,
    /// Captured at show time; used by the dismiss task to detect staleness.
    generation: u64,
    /// Monotonic id used when no explicit id was supplied. Ensures unique
    /// transition ids across auto-generated notifications.
    slot: u64,
}

impl NotificationEntry {
    fn transition_id(&self, suffix: &str) -> ElementId {
        let key: SharedString = if self.notification.id.is_empty() {
            format!("__notif-{}{}", self.slot, suffix).into()
        } else {
            format!("{}{}", self.notification.id, suffix).into()
        };
        ElementId::Name(key)
    }
}

/// Default auto-dismiss duration when none is specified.
const DEFAULT_DURATION: Duration = Duration::from_secs(4);

/// How long the dismiss animation runs before the entry is dropped. Should
/// match the `transition_when_else` duration used in `render_notification`.
const NOTIFICATION_DISMISS_MS: u64 = 300;

// ---------------------------------------------------------------------------
// NotificationController — global host
// ---------------------------------------------------------------------------

/// Global host for toast notifications. Rendered at the app root so toasts
/// overlay the entire window regardless of which view is active. Holds up
/// to [`MAX_NOTIFICATIONS`] simultaneously-live toasts; older ones are
/// dropped (FIFO) when the cap is exceeded.
pub struct NotificationController {
    entries: Vec<NotificationEntry>,
    /// Monotonic counter assigned to each new entry. Used both for unique
    /// transition ids and as the staleness guard captured by dismiss tasks.
    generation: u64,
    position: NotificationPosition,
}

/// Maximum number of toasts shown at once. Excess toasts (oldest first) are
/// dropped immediately when `show` is called beyond this cap.
const MAX_NOTIFICATIONS: usize = 4;

impl NotificationController {
    pub fn new(position: NotificationPosition) -> Self {
        Self {
            entries: Vec::new(),
            generation: 0,
            position,
        }
    }

    /// Builder-style position override for callers constructing the controller
    /// with the default `Entity::new` pattern.
    pub fn with_position(mut self, position: NotificationPosition) -> Self {
        self.position = position;
        self
    }

    /// Show a notification. If the cap is exceeded the oldest open toast is
    /// dropped (no dismiss animation) to make room — prefer dismissing
    /// explicitly when the user has already seen a toast.
    pub fn show(&mut self, notification: Notification, cx: &mut Context<Self>) {
        // Bump the global generation and assign a slot-specific generation.
        self.generation = self.generation.wrapping_add(1);
        let slot = self.generation;

        let entry = NotificationEntry {
            notification,
            open: true,
            generation: slot,
            slot,
        };

        // Enforce the simultaneous cap. Drop the oldest entry without an
        // animation — it's been on screen the longest so this is the
        // least-surprising eviction. (Transition state for its id is reset
        // so a future toast reusing the same caller-supplied id animates in
        // from scratch rather than rendering at the dismiss endpoint.)
        if self.entries.len() >= MAX_NOTIFICATIONS {
            if let Some(dropped) = self.entries.first() {
                let card_id = dropped.transition_id("-card");
                gpui_animation::reset_transition(&card_id);
            }
            self.entries.remove(0);
        }

        self.entries.push(entry);
        cx.notify();

        // Schedule auto-dismiss. We capture the entry's generation at
        // show time; `reset_auto_dismiss` bumps each open entry's generation
        // and schedules a fresh timer, so when this original task fires it
        // can detect that the user hovered and bail out (leaving the toast
        // on screen for the reset timer to handle). `slot`
        // equals the show-time generation, so we reuse it as the captured
        // value here.
        let duration = self
            .entries
            .last()
            .map(|e| e.notification.duration)
            .unwrap_or(DEFAULT_DURATION);
        if !duration.is_zero() {
            let entity = cx.entity().downgrade();
            let dismiss_slot = slot;
            let dismiss_gen = slot;
            cx.spawn(async move |_this, cx| {
                smol::Timer::after(duration).await;
                let _ = entity.update(cx, |this, cx| {
                    this.begin_dismiss_slot(dismiss_slot, Some(dismiss_gen), cx);
                });
            })
            .detach();
        }
    }

    /// Begin the dismiss animation for the toast matching `slot` (the
    /// internal counter). Pass `expected_generation = Some(gen)` to bail out
    /// if the entry's generation has since changed (e.g. the user hovered and
    /// `reset_auto_dismiss` bumped it) — this is what the auto-dismiss task
    /// uses. Pass `None` to dismiss unconditionally (close button, public
    /// `dismiss(id)`, action callbacks).
    fn begin_dismiss_slot(
        &mut self,
        slot: u64,
        expected_generation: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.slot == slot) {
            let stale = expected_generation.is_some_and(|g| g != entry.generation);
            if stale {
                // Stale auto-dismiss (user hovered) — leave the toast open.
                return;
            }
            if entry.open {
                entry.open = false;
                cx.notify();
            }
            self.schedule_cleanup(slot, cx);
        }
    }

    /// Begin the dismiss animation for the toast whose caller-supplied id
    /// matches `id`. No-op if no such toast is currently open. Use this from
    /// action callbacks or to programmatically close a known toast.
    pub fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        let slot = self
            .entries
            .iter()
            .find(|e| e.notification.id.as_ref() == id && e.open)
            .map(|e| e.slot);
        if let Some(slot) = slot {
            self.begin_dismiss_slot(slot, None, cx);
        }
    }

    /// Drop the entry after the out-animation has had time to play. Same
    /// generation-guard pattern as `ContextMenuController::begin_dismiss`.
    fn schedule_cleanup(&mut self, slot: u64, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(Duration::from_millis(NOTIFICATION_DISMISS_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                // Only remove the entry if it hasn't been re-shown (its
                // generation would have changed) and is still present.
                let idx = this.entries.iter().position(|e| e.slot == slot);
                if let Some(idx) = idx {
                    // Reset transition state for the dropped card so a future
                    // toast that reuses the same caller-supplied id animates
                    // in from scratch rather than starting at opacity 0.
                    let card_id = this.entries[idx].transition_id("-card");
                    gpui_animation::reset_transition(&card_id);
                    this.entries.remove(idx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Returns `true` when at least one toast is showing or dismissing.
    pub fn is_active(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Reset the auto-dismiss timer for all currently-open toasts (e.g. on
    /// hover). Cancels the in-flight timer by bumping the generation, then
    /// schedules a fresh one so the toast gets a full `duration` of additional
    /// time. The toast will still auto-dismiss — it just gets more time.
    pub fn reset_auto_dismiss(&mut self, cx: &mut Context<Self>) {
        let mut slots_to_reset: Vec<(u64, u64, Duration)> = Vec::new();
        for entry in self.entries.iter_mut() {
            if entry.open && !entry.notification.duration.is_zero() {
                self.generation = self.generation.wrapping_add(1);
                entry.generation = self.generation;
                slots_to_reset.push((entry.slot, entry.generation, entry.notification.duration));
            }
        }
        if slots_to_reset.is_empty() {
            return;
        }
        cx.notify();

        for (slot, reset_gen, duration) in slots_to_reset {
            let entity = cx.entity().downgrade();
            cx.spawn(async move |_this, cx| {
                smol::Timer::after(duration).await;
                let _ = entity.update(cx, |this, cx| {
                    this.begin_dismiss_slot(slot, Some(reset_gen), cx);
                });
            })
            .detach();
        }
    }
}

impl Render for NotificationController {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.entries.is_empty() {
            return div().into_any_element();
        }

        let position = self.position;
        let weak = _cx.entity().downgrade();

        // Snapshot the entries we need to render. We clone the lightweight
        // per-toast data out so the render helpers don't borrow `self`.
        let snapshotted: Vec<(
            NotificationEntry,
            Rc<dyn Fn(&str, &mut Window, &mut App) + 'static>,
        )> = self
            .entries
            .iter()
            .cloned()
            .map(|e| {
                let weak = weak.clone();
                let slot = e.slot;
                let close_cb: Rc<dyn Fn(&str, &mut Window, &mut App) + 'static> =
                    Rc::new(move |_id: &str, _w: &mut Window, cx: &mut App| {
                        let _ = weak.update(cx, |this, cx| {
                            this.begin_dismiss_slot(slot, None, cx);
                        });
                    });
                (e, close_cb)
            })
            .collect();

        render_notification_stack(position, weak.clone(), snapshotted).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_notification_stack(
    position: NotificationPosition,
    controller: WeakEntity<NotificationController>,
    entries: Vec<(
        NotificationEntry,
        Rc<dyn Fn(&str, &mut Window, &mut App) + 'static>,
    )>,
) -> impl IntoElement {
    // Absolute full-size layer that does NOT occlude — toasts are non-modal
    // and must let the underlying UI receive pointer events. We use a
    // pointer-events-none container and re-enable pointer events on each
    // card so the gap between cards doesn't swallow clicks.
    let mut layer = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .p_4()
        .gap_2();

    match position {
        NotificationPosition::BottomRight => {
            // Push cards to the bottom, then align them to the right.
            layer = layer.flex_col_reverse().items_end().justify_end();
        }
        NotificationPosition::TopCenter => {
            layer = layer.flex_col().items_center().justify_start();
        }
    }

    for (entry, close_cb) in entries {
        layer = layer.child(render_notification_card(
            entry,
            controller.clone(),
            close_cb,
        ));
    }

    layer
}

#[allow(clippy::too_many_arguments)]
fn render_notification_card(
    entry: NotificationEntry,
    controller: WeakEntity<NotificationController>,
    close_cb: Rc<dyn Fn(&str, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let open = entry.open;
    let level = entry.notification.level;
    let title = entry.notification.title.clone();
    let message = entry.notification.message.clone();
    let action_label = entry.notification.action_label.clone();
    let action = entry.notification.action.clone();
    let accent = level.accent();
    let icon_path = level.icon();

    let card_id = entry.transition_id("-card");

    let has_action = action_label.is_some() && action.is_some();

    // The card itself. Initial hidden state (opacity 0 + slight upward
    // translate) is animated to visible by the transition below.
    let mut card = div()
        .id(card_id.clone())
        .w(px(360.0))
        // Re-enable pointer events on the card so it's clickable even though
        // the surrounding layer is click-through.
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded(RADIUS_MD)
        .shadow_lg()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .p_3()
        // Initial hidden state — transition animates these to visible.
        .opacity(0.0)
        .mt(px(-8.0))
        .with_transition(card_id)
        .transition_when_else(
            open,
            Duration::from_millis(NOTIFICATION_DISMISS_MS),
            EASE_OUT,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-8.0)),
        );

    // Stop clicks on the card from doing anything unexpected (the layer is
    // non-occluding so there's nothing to bubble to, but being explicit is
    // cheap and matches the dialog/context-menu pattern).
    if open {
        card = card.on_click(|_e, _w, cx| {
            cx.stop_propagation();
        });
    }

    // Reset auto-dismiss while the user is hovering — standard toast UX so
    // the message doesn't vanish while you're reading it. Hovering bumps
    // every open entry's generation (invalidating any in-flight auto-dismiss
    // task) and schedules a fresh timer, so the toast gets a full additional
    // `duration` rather than being permanently paused.
    if open {
        card = card.on_hover(move |hovered, _w, cx| {
            if *hovered {
                let _ = controller.update(cx, |this, cx| {
                    this.reset_auto_dismiss(cx);
                });
            }
        });
    }

    // Leading icon (accent-colored).
    card = card.child(
        svg()
            .path(icon_path)
            .size(px(18.0))
            .flex_shrink_0()
            .text_color(rgb(accent)),
    );

    // Body: title + optional message, takes the remaining width.
    let mut body = div().flex().flex_col().gap_0p5().min_w_0().flex_1().child(
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(text_primary()))
            .child(title.to_string()),
    );
    if let Some(msg) = message {
        body = body.child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                // Allow long messages to wrap inside the card.
                .whitespace_normal()
                .child(msg.to_string()),
        );
    }

    card = card.child(body);

    // Trailing column: optional action button + always-available close.
    let mut trailing = div().flex().flex_col().items_end().gap_1().flex_none();

    if has_action {
        let label = action_label.unwrap();
        let action = action.unwrap();
        let action_id = entry.transition_id("-action");
        trailing = trailing.child(
            div()
                .id(action_id)
                .px_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(accent))
                .child(label.to_string())
                .on_click(move |_e, w, cx| {
                    action(w, cx);
                    cx.stop_propagation();
                }),
        );
    }

    // Close button (×). Always rendered so the user has an explicit escape
    // hatch even when auto-dismiss is on.
    let close_id = entry.transition_id("-close");
    let close_id_for_cb = entry.notification.id.clone();
    trailing = trailing.child(
        div()
            .id(close_id)
            .flex()
            .items_center()
            .justify_center()
            .h_5()
            .w_5()
            .rounded(RADIUS_XS)
            .text_color(rgb(text_muted()))
            .child(
                svg()
                    .path("icons/close.svg")
                    .size_3()
                    .text_color(rgb(text_muted())),
            )
            .on_click(move |_e, w, cx| {
                close_cb(close_id_for_cb.as_ref(), w, cx);
                cx.stop_propagation();
            }),
    );

    card.child(trailing)
}
