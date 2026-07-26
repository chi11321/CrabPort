//! Motion & layout tokens — the single source of truth for animation
//! durations, easing curves, and corner radii across the whole UI.
//!
//! ## Why a tokens module?
//!
//! Before this module, durations and easings were sprinkled as inline
//! `Duration::from_millis(...)` and ad-hoc `Linear` / `EaseInOutQuad` /
//! `EaseInOutCubic` / `EaseOutQuad` choices at every call site. That made
//! the UI feel inconsistent — a dropdown opened in 250 ms on `EaseInOutQuad`
//! while the dialog next to it faded in 150 ms on `Linear`, and the context
//! menu mixed both. The values also drifted over time (hover transitions
//! ranged from 100 ms to 120 ms to 150 ms depending on the file).
//!
//! This module centralizes them. Call sites should pull a named token
//! instead of inventing a number:
//!
//! ```ignore
//! use gpui_animation::animation::TransitionExt;
//! use crate::motion::{duration_base, EASE_STANDARD};
//!
//! div()
//!     .transition_when_else(
//!         open,
//!         duration_base(),
//!         EASE_STANDARD,
//!         |el| el.opacity(1.0),
//!         |el| el.opacity(0.0),
//!     )
//! ```
//!
//! ## Token taxonomy
//!
//! Durations are tuned for an interactive desktop app (≈120 Hz UI thread).
//! Every `duration_*()` token reads the live speed multiplier from
//! [`set_speed_multiplier`], so changing [`AnimationSpeed`] in config scales
//! them uniformly:
//!
//! | Token                | Baseline ms | Use for                                    |
//! |----------------------|-------------|--------------------------------------------|
//! | `duration_instant()` | 0           | State changes that should appear immediate |
//! |                      |             | but still ride the transition cache (e.g.  |
//! |                      |             | disabled text color).                      |
//! | `duration_fast()`    | 100         | Micro-interactions: hover bg / opacity on  |
//! |                      |             | list rows, menu items, small chips.        |
//! | `duration_base()`    | 150         | Default for most state transitions: dialog |
//! |                      |             | fade, overlay dim, input border, tooltip.  |
//! | `duration_moderate()`| 200         | Open/close transitions with rotation or    |
//! |                      |             | height changes: chevrons, collapsible rows.|
//! | `duration_slow()`    | 250         | Larger state flips: dropdown menu open,    |
//! |                      |             | segmented control indicator, button bg.    |
//! | `duration_slower()`  | 320         | Tab indicator slide (multi-step layout     |
//! |                      |             | animation).                                |
//!
//! Easings:
//!
//! - [`EASE_STANDARD`] — `EaseInOutCubic`. The default for almost every
//!   transition. Symmetric accel/decel feels natural for state toggles.
//! - [`EASE_OUT`] — `EaseOutQuad`. For elements entering the viewport
//!   (drop-in, fade-in): starts fast, settles gently.
//! - [`EASE_LINEAR`] — `Linear`. Reserved for progress bars and
//!   continuous-value animations (memory meter, transfer progress) where
//!   easing would feel laggy.
//!
//! Corner radii (use these in place of raw `rounded_*` Tailwind helpers so
//! the visual language stays consistent):
//!
//! | Token         | px | Replaces     | Use for                            |
//! |---------------|----|--------------|------------------------------------|
//! | `RADIUS_XS`   | 2  | `rounded_sm` | Tight inset chips (segment inside  |
//! |               |    |              | a container, dropdown items).      |
//! | `RADIUS_SM`   | 4  | `rounded_md` | Small standalone elements (icon    |
//! |               |    |              | buttons, star toggle).             |
//! | `RADIUS_MD`   | 6  | `rounded_md` | Default for containers (inputs,    |
//! |               |    |              | dropdowns, buttons, tab chips).    |
//! | `RADIUS_LG`   | 8  | `rounded_lg` | Dialogs, command palette, host      |
//! |               |    |              | selector, large surfaces.          |
//! | `RADIUS_FULL` | n/a| `rounded_full`| Pills, dots, knobs.               |
//!
//! Radii are exposed as `gpui::DefiniteLength` so they plug straight into
//! `.rounded(...)`:
//!
//! ```ignore
//! use crate::motion::RADIUS_MD;
//!
//! div().rounded(RADIUS_MD)
//! ```

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui::Pixels;
use gpui_animation::transition::general::{EaseInOutCubic, EaseOutQuad, Linear};

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------
//
// Every `DURATION_*` constant is the *baseline* ("Standard" speed tier)
// value. `duration_*()` returns the live value scaled by the current
// global multiplier (set from `AppearanceConfig::animation_speed` at app
// init and on Settings changes). Call sites should call the function, not
// read the constant — the constant only exists as the tuned reference and
// as the unit-test oracle.
//
// The multiplier is stored as an integer `millis × 1000` (microseconds per
// baseline millisecond) in an `AtomicU32` so motion tokens can be queried
// from any thread without a lock. `1.0×` is stored as `1000` (i.e. every
// baseline ms yields 1000 µs = 1 ms). `0.75×` stores `750`, `1.25×` stores
// `1250`, etc. This sidesteps the lack of `AtomicF32` / `AtomicF64` in
// `std` and keeps the hot path (a `Duration::from_micros` per call)
// allocation-free.

/// Global animation speed multiplier, encoded as `millis × 1000` (µs per
/// baseline ms). `1000` = 1.0×. Set once at app init from
/// `AppearanceConfig::animation_speed` and updated whenever the user
/// changes the Settings dropdown. Reads happen on every `duration_*()`
/// call, so `Ordering::Relaxed` is enough — we only need eventual
/// visibility, not cross-field synchronization.
static SPEED_MICROS_PER_MS: AtomicU32 = AtomicU32::new(1000);

/// Apply the live multiplier to a baseline `Duration`. Returns the scaled
/// duration; called by every `duration_*()` below.
///
/// `Duration` doesn't expose `* f32` directly, so we scale the underlying
/// nanosecond count. `as_nanos` is `u128`, which doesn't overflow for any
/// plausible baseline × multiplier (a 500 ms baseline at 1.25× is 625 ms =
/// 625_000_000 ns, far below `u128::MAX`).
fn scale(base: Duration) -> Duration {
    let per_ms = SPEED_MICROS_PER_MS.load(Ordering::Relaxed) as f32 / 1000.0;
    let nanos = base.as_nanos() as f64;
    Duration::from_nanos((nanos * per_ms as f64).round() as u64)
}

/// Set the global animation speed multiplier (1.0 = baseline). Called at
/// app init from `config::snapshot().appearance.animation_speed.multiplier()`
/// and again whenever the user changes the Settings dropdown. Passing a
/// non-finite or non-positive value is a no-op so a corrupted config can't
/// zero-out every animation.
pub fn set_speed_multiplier(multiplier: f32) {
    if !multiplier.is_finite() || multiplier <= 0.0 {
        return;
    }
    // Store as µs-per-ms (1000 = 1.0×) so the `scale` fast path stays in
    // integer arithmetic.
    SPEED_MICROS_PER_MS.store((multiplier * 1000.0).round() as u32, Ordering::Relaxed);
}

/// Baseline 0 ms ("Standard" tier). For state changes that should appear
/// immediate but still ride the transition cache (e.g. flipping a
/// `disabled` text color). Scaling a zero duration stays zero, so this
/// token is effectively unaffected by `animation_speed`.
pub const DURATION_INSTANT: Duration = Duration::from_millis(0);
/// Live version of [`DURATION_INSTANT`] — apply the current speed
/// multiplier. Always returns zero (scaling a zero duration is a no-op),
/// but kept for symmetry with the other `duration_*` tokens so call sites
/// stay consistent.
pub fn duration_instant() -> Duration {
    scale(DURATION_INSTANT)
}

/// Baseline 100 ms ("Standard" tier) — micro-interactions: hover bg /
/// opacity on list rows, menu items, small chips.
pub const DURATION_FAST: Duration = Duration::from_millis(100);
/// Live version of [`DURATION_FAST`] — apply the current speed multiplier.
pub fn duration_fast() -> Duration {
    scale(DURATION_FAST)
}

/// Baseline 150 ms ("Standard" tier) — default for most state transitions:
/// dialog fade, overlay dim, input border, tooltip.
pub const DURATION_BASE: Duration = Duration::from_millis(150);
/// Live version of [`DURATION_BASE`] — apply the current speed multiplier.
pub fn duration_base() -> Duration {
    scale(DURATION_BASE)
}

/// Baseline 200 ms ("Standard" tier) — open/close transitions with
/// rotation or height changes: chevrons, collapsible row sections.
pub const DURATION_MODERATE: Duration = Duration::from_millis(200);
/// Live version of [`DURATION_MODERATE`] — apply the current speed
/// multiplier.
pub fn duration_moderate() -> Duration {
    scale(DURATION_MODERATE)
}

/// Baseline 250 ms ("Standard" tier) — larger state flips: dropdown menu
/// open, segmented control indicator slide, button background.
pub const DURATION_SLOW: Duration = Duration::from_millis(250);
/// Live version of [`DURATION_SLOW`] — apply the current speed multiplier.
pub fn duration_slow() -> Duration {
    scale(DURATION_SLOW)
}

/// Baseline 320 ms ("Standard" tier) — tab indicator slide (multi-step
/// layout animation).
pub const DURATION_SLOWER: Duration = Duration::from_millis(320);
/// Live version of [`DURATION_SLOWER`] — apply the current speed
/// multiplier.
pub fn duration_slower() -> Duration {
    scale(DURATION_SLOWER)
}

// ---------------------------------------------------------------------------
// Easings
// ---------------------------------------------------------------------------
// Easing structs in `gpui_animation::transition::general` are zero-sized
// unit structs, so a single const value of each is enough — callers pass
// it by value (it `Copy`s for free) into the `transition_*` APIs. Use these
// consts instead of importing the raw easing struct so the motion module
// stays the single touch point.

/// The default for almost every transition. Symmetric accel/decel feels
/// natural for state toggles (open/close, hover in/out).
pub const EASE_STANDARD: EaseInOutCubic = EaseInOutCubic;

/// For elements entering the viewport (drop-in, fade-in): starts fast,
/// settles gently. Use for one-directional enter animations; pair with
/// [`EASE_STANDARD`] when the same element also animates out.
pub const EASE_OUT: EaseOutQuad = EaseOutQuad;

/// Reserved for progress bars and continuous-value animations (memory
/// meter, transfer progress) where easing would feel laggy.
pub const EASE_LINEAR: Linear = Linear;

// ---------------------------------------------------------------------------
// Corner radii
// ---------------------------------------------------------------------------
/// Type returned by every radius token. `Pixels` converts into any
/// `impl Into<AbsoluteLength>` site (including the `Styled::rounded(...)`
/// setter), so the tokens can be passed straight through.
///
/// Prefer these over raw `rounded_sm` / `rounded_md` / `rounded_lg` Tailwind
/// helpers so the visual language stays consistent across surfaces.
pub type Radius = Pixels;

/// 2 px — tight inset chips (segment inside a container, dropdown items,
/// close-icon backgrounds). Drop-in for `rounded_sm` on small inset
/// surfaces.
pub const RADIUS_XS: Radius = gpui::px(2.0);

/// 4 px — small standalone elements (icon buttons, star toggle, segmented
/// control indicator, dropdown items).
pub const RADIUS_SM: Radius = gpui::px(4.0);

/// 6 px — default for containers (inputs, dropdowns, buttons, tab chips,
/// list rows, keybind chips). Slightly rounder than the previous 4 px
/// default so borders read softer at small sizes.
pub const RADIUS_MD: Radius = gpui::px(6.0);

/// 8 px — dialogs, command palette, host selector, large surfaces.
pub const RADIUS_LG: Radius = gpui::px(8.0);

/// Fully rounded — pills, status dots, switch knobs. Use with
/// `.rounded_full()` (the Tailwind helper) rather than a numeric value,
/// since fully-round needs to read the element's own dimension.
pub const RADIUS_FULL: Radius = gpui::px(9999.0);
