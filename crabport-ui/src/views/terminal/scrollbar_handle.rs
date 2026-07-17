//! Bridge between the `gpui_component::scroll::Scrollbar` widget and the
//! alacritty grid scrolling model used by [`crate::views::terminal::TerminalView`].
//!
//! The standard `Scrollbar` works in **pixels** via the [`ScrollbarHandle`]
//! trait (`offset().y` is a pixel offset where `0` = content-top visible and
//! `-(content_height - container_height)` = content-bottom visible).
//!
//! The terminal grid works in **rows**: `display_offset` (`0` = bottom/newest,
//! `history_size` = top/oldest). [`TerminalScrollbarHandle`] adapts between
//! the two by:
//!
//! - Reading the current `display_offset` / `history_size` / `visible_rows`
//!   from atomics kept fresh by the terminal's prepaint loop, and computing
//!   `offset()` / `content_size()` in pixels using `line_height`.
//! - Translating `set_offset(pixel_offset)` into a row delta and forwarding
//!   it to `TerminalSession::scroll(delta)`, updating the local atomic so
//!   the next render sees the new offset before prepaint re-syncs it.
//!
//! This lets the terminal use the exact same `Scrollbar` component as the rest
//! of the app (history panel, SFTP panel, snippets panel, …), so styles,
//! hover/drag behavior, hit-testing, and fade animations stay consistent.

use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use gpui::{Pixels, Point, Size, point, px};
use gpui_component::scroll::ScrollbarHandle;
use parking_lot::Mutex;

use crabport_terminal::terminal::TerminalSession;

/// A scrollbar handle that drives an alacritty terminal grid.
///
/// All state is shared (Arc / atomics) so the handle can be cloned cheaply
/// and stays in sync with [`crate::views::terminal::TerminalView`]'s
/// `display_offset` / `history_size` / `visible_rows` / `last_bounds`
/// atomics, which are updated every prepaint.
#[derive(Clone)]
pub struct TerminalScrollbarHandle {
    /// The terminal session to drive (`scroll` takes a row delta).
    session: Arc<TerminalSession>,
    /// Current display offset (rows scrolled up from the bottom).
    /// Mirrors `TerminalView::display_offset`. Updated by prepaint on every
    /// frame and by `set_offset` during a drag (so the next render shows the
    /// drag's progress immediately, before prepaint re-syncs it).
    display_offset: Arc<AtomicI32>,
    /// Total scrollback history, in rows. Updated by prepaint.
    history_size: Arc<AtomicI32>,
    /// Number of visible rows in the terminal viewport. Updated by prepaint.
    visible_rows: Arc<AtomicI32>,
    /// Per-row pixel height. Wrapped in a `Mutex` because `Pixels` isn't
    /// `Atomic`-friendly — set once per render before constructing the
    /// scrollbar and read in `offset()` / `content_size()`.
    line_height: Arc<Mutex<Pixels>>,
}

impl TerminalScrollbarHandle {
    /// Construct from existing atomics shared with [`crate::views::terminal::TerminalView`].
    /// The prepaint loop writes to these each frame; this handle reads them
    /// to compute `offset()` / `content_size()` for the scrollbar widget.
    pub fn new_from_atomics(
        session: Arc<TerminalSession>,
        display_offset: Arc<AtomicI32>,
        history_size: Arc<AtomicI32>,
        visible_rows: Arc<AtomicI32>,
    ) -> Self {
        Self {
            session,
            display_offset,
            history_size,
            visible_rows,
            line_height: Arc::new(Mutex::new(px(0.0))),
        }
    }

    /// Update the per-row pixel height. Called from `render` after reading
    /// the live `TerminalView::line_height`.
    pub fn set_line_height(&self, lh: Pixels) {
        *self.line_height.lock() = lh;
    }
}

impl ScrollbarHandle for TerminalScrollbarHandle {
    fn offset(&self) -> Point<Pixels> {
        let lh = *self.line_height.lock();
        // Guard against zero to avoid NaNs before the first paint.
        if lh <= px(0.0) {
            return point(px(0.0), px(0.0));
        }
        let lh_f = f32::from(lh);
        let history = self.history_size.load(Ordering::Relaxed).max(0) as f32;
        let visible = self.visible_rows.load(Ordering::Relaxed).max(1) as f32;
        let offset = self.display_offset.load(Ordering::Relaxed).max(0) as f32;

        // Total content height in pixels.
        let content_h = (history + visible) * lh_f;
        let container_h = visible * lh_f;
        // Scrollbar convention: offset.y = 0 when content-top is visible,
        // offset.y = -(content_h - container_h) when content-bottom is visible.
        //
        // Terminal convention: display_offset = 0 means bottom (newest), so
        // the viewport is already at content-bottom → offset.y should be
        // -(content_h - container_h). display_offset = history means top
        // (oldest) → offset.y = 0.
        //
        // So: offset.y = -(content_h - container_h) + display_offset * lh
        let max_offset = (content_h - container_h).max(0.0);
        let y = -(max_offset - offset * lh_f);
        point(px(0.0), px(y))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let lh = *self.line_height.lock();
        if lh <= px(0.0) {
            return;
        }
        let lh_f = f32::from(lh);
        let history = self.history_size.load(Ordering::Relaxed).max(0) as f32;
        let visible = self.visible_rows.load(Ordering::Relaxed).max(1) as f32;
        let content_h = (history + visible) * lh_f;
        let container_h = visible * lh_f;
        let max_offset = (content_h - container_h).max(0.0);

        // Invert the offset() mapping: new display_offset (rows from bottom)
        // = (max_offset + offset.y) / lh.
        let new_y_px = (max_offset + f32::from(offset.y)).max(0.0);
        let new_display_offset = (new_y_px / lh_f).round().clamp(0.0, history) as i32;

        let cur = self.display_offset.load(Ordering::Relaxed);
        let delta = new_display_offset - cur;
        if delta == 0 {
            return;
        }
        // Update the local atomic immediately so subsequent `offset()` calls
        // during the same drag gesture see the new value (the Scrollbar
        // widget reads offset() on the next mousemove to decide whether to
        // emit another set_offset). Prepaint will re-sync this from the
        // alacritty grid shortly after.
        self.display_offset
            .store(new_display_offset, Ordering::Relaxed);
        self.session.scroll(delta);
    }

    fn content_size(&self) -> Size<Pixels> {
        let lh = *self.line_height.lock();
        if lh <= px(0.0) {
            return Size::default();
        }
        let history = self.history_size.load(Ordering::Relaxed).max(0) as f32;
        let visible = self.visible_rows.load(Ordering::Relaxed).max(1) as f32;
        // Width is unused for a vertical-only scrollbar; height is the
        // full content (history + visible rows).
        Size {
            width: px(0.0),
            height: px((history + visible) * f32::from(lh)),
        }
    }

    fn start_drag(&self) {
        // No special drag setup needed — `set_offset` handles everything.
    }

    fn end_drag(&self) {
        // Snap back to the grid's actual display_offset in case the drag
        // landed between rows. Prepaint will re-sync on the next frame.
        let _ = self.session.with_term(|term| {
            let actual = term.grid().display_offset() as i32;
            self.display_offset.store(actual, Ordering::Relaxed);
        });
    }
}
