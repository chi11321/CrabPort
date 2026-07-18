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
//! use crate::motion::{DURATION_BASE, EASE_STANDARD};
//!
//! div()
//!     .transition_when_else(
//!         open,
//!         DURATION_BASE,
//!         EASE_STANDARD,
//!         |el| el.opacity(1.0),
//!         |el| el.opacity(0.0),
//!     )
//! ```
//!
//! ## Token taxonomy
//!
//! Durations are tuned for an interactive desktop app (≈120 Hz UI thread):
//!
//! | Token                | ms  | Use for                                       |
//! |----------------------|-----|-----------------------------------------------|
//! | `DURATION_INSTANT`   | 0   | State changes that should appear immediate    |
//! |                       |     | but still ride the transition cache (e.g.     |
//! |                       |     | disabled text color).                         |
//! | `DURATION_FAST`      | 100 | Micro-interactions: hover bg / opacity on     |
//! |                       |     | list rows, menu items, small chips.           |
//! | `DURATION_BASE`      | 150 | Default for most state transitions: dialog    |
//! |                       |     | fade, overlay dim, input border, tooltip.     |
//! | `DURATION_MODERATE`  | 200 | Open/close transitions with rotation or       |
//! |                       |     | height changes: chevrons, collapsible rows.   |
//! | `DURATION_SLOW`      | 250 | Larger state flips: dropdown menu open,       |
//! |                       |     | segmented control indicator, button bg.       |
//! | `DURATION_SLOWER`    | 320 | Tab indicator slide (multi-step layout        |
//! |                       |     | animation).                                   |
//! | `DURATION_VERY_SLOW` | 500 | Big layout mutations: sidebar/panel width,    |
//! |                       |     | connection overlay fade-out.                  |
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

use std::time::Duration;

use gpui::Pixels;
use gpui_animation::transition::general::{EaseInOutCubic, EaseOutQuad, Linear};

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

/// 0 ms — for state changes that should appear immediate but still ride
/// the transition cache (e.g. flipping a `disabled` text color).
pub const DURATION_INSTANT: Duration = Duration::from_millis(0);

/// 100 ms — micro-interactions: hover bg / opacity on list rows, menu
/// items, small chips.
pub const DURATION_FAST: Duration = Duration::from_millis(100);

/// 150 ms — default for most state transitions: dialog fade, overlay dim,
/// input border, tooltip.
pub const DURATION_BASE: Duration = Duration::from_millis(150);

/// 200 ms — open/close transitions with rotation or height changes:
/// chevrons, collapsible row sections.
pub const DURATION_MODERATE: Duration = Duration::from_millis(200);

/// 250 ms — larger state flips: dropdown menu open, segmented control
/// indicator slide, button background.
pub const DURATION_SLOW: Duration = Duration::from_millis(250);

/// 320 ms — tab indicator slide (multi-step layout animation).
pub const DURATION_SLOWER: Duration = Duration::from_millis(320);

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
