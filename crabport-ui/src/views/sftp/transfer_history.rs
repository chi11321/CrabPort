//! Toggleable SFTP transfer-history popover.
//!
//! Like the global tooltip / context-menu controllers, this is an
//! `Entity` held by the app root and rendered as a top-level overlay
//! (not inside the `SftpTabView` view tree). The SFTP toolbar's history
//! button calls `toggle` on click; the popover anchors to the button's
//! position and shows the most recent completed transfers.
//!
//! Records are pushed by `SftpTabView` via [`TransferHistoryController::push`]
//! whenever an `on_sftp_transfer_finished` callback fires. They live in
//! memory only — see `TransferRecord` for details.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crate::color::*;
use crate::motion::{DURATION_BASE, EASE_STANDARD, RADIUS_MD};
use crabport_terminal::terminal::SftpTransferKind;

/// Maximum records kept in memory. Older entries are dropped (FIFO).
const MAX_RECORDS: usize = 200;
/// Pop-in/out animation duration. Must match the `transition_when_else`
/// duration in `render_transfer_history` so the dismiss timer clears
/// state at the right moment.
const DISMISS_MS: u64 = 180;

/// A completed SFTP transfer, captured for the history popover.
///
/// Records are kept in-memory only (one `Vec` per controller), most-
/// recent-first. We don't persist them to the Store — the history is
/// meant as a "what just happened" aid within a session, not a
/// permanent log.
#[derive(Clone, Debug)]
pub struct TransferRecord {
    pub kind: SftpTransferKind,
    pub success: bool,
    /// The backend-supplied detail string — typically the path of the
    /// file that was processed. Kept verbatim so the user sees exactly
    /// what the backend reported.
    pub message: String,
    /// When the transfer finished (local Unix timestamp), for display.
    pub finished_at: i64,
}

pub struct TransferHistoryController {
    /// Most-recent-first. Capped at [`MAX_RECORDS`].
    records: Vec<TransferRecord>,
    /// Where to anchor the popover (button's window-relative position).
    anchor: Option<Point<Pixels>>,
    open: bool,
    /// Monotonic counter bumped on every show/hide so stale dismiss
    /// tasks don't clobber a freshly-shown popover (mirrors the
    /// context-menu controller's `generation` pattern).
    generation: u64,
}

impl TransferHistoryController {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            anchor: None,
            open: false,
            generation: 0,
        }
    }

    /// Append a record. Most-recent-first ordering; oldest entries are
    /// evicted past `MAX_RECORDS`. If the popover is currently open, it
    /// re-renders to show the new entry.
    pub fn push(&mut self, record: TransferRecord, cx: &mut Context<Self>) {
        self.records.insert(0, record);
        if self.records.len() > MAX_RECORDS {
            self.records.truncate(MAX_RECORDS);
        }
        cx.notify();
    }

    /// Toggle the popover open/closed at `anchor` (window-relative).
    pub fn toggle(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        if self.open {
            self.hide(cx);
        } else {
            self.show(anchor, cx);
        }
    }

    /// Show the popover at `anchor`. Replaces any currently-showing
    /// popover (re-anchors it).
    pub fn show(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.anchor = Some(anchor);
        self.open = true;
        // Reset the animation so a re-show after a dismiss animates in
        // from scratch (mirrors the context-menu controller).
        gpui_animation::reset_transition(&ElementId::Name("sftp-transfer-history-popover".into()));
        cx.notify();
    }

    /// Begin the dismiss animation. After [`DISMISS_MS`] the state is
    /// dropped so the popover no longer captures renders.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let dismiss_gen = self.generation;
        self.open = false;
        cx.notify();

        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(DISMISS_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                if this.generation == dismiss_gen {
                    this.anchor = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Number of records currently held (for badge display, etc.).
    pub fn len(&self) -> usize {
        self.records.len()
    }
}

impl Render for TransferHistoryController {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(anchor) = self.anchor else {
            return div().into_any_element();
        };
        let open = self.open;
        let records = self.records.clone();
        let viewport = _window.viewport_size();
        let entity = cx.entity().downgrade();

        render_transfer_history(anchor, open, records, viewport, entity).into_any_element()
    }
}

/// Render the popover. Anchored to `anchor` (the toggle button's
/// position), clamped to stay inside the window.
fn render_transfer_history(
    anchor: Point<Pixels>,
    open: bool,
    records: Vec<TransferRecord>,
    viewport: Size<Pixels>,
    entity: WeakEntity<TransferHistoryController>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("sftp-transfer-history-overlay".into());
    let popover_id = ElementId::Name("sftp-transfer-history-popover".into());

    // Clamp the popover so it stays inside the window. Fixed height
    // so the popover doesn't resize as records come in — the list
    // inside scrolls instead. Width is fixed at 360px.
    const POPOVER_W: f32 = 360.0;
    const POPOVER_H: f32 = 240.0;
    let mut x = f32::from(anchor.x);
    let mut y = f32::from(anchor.y) + 28.0; // below the button
    // Flip left: if the popover would overflow the right edge, place it
    // to the left of the anchor so its right edge aligns with the button.
    if x + POPOVER_W > f32::from(viewport.width) {
        x = (f32::from(anchor.x) - POPOVER_W).max(0.0);
    }
    // Flip up: if the popover would overflow the bottom edge, place it
    // above the anchor so its bottom edge aligns with the button.
    if y + POPOVER_H > f32::from(viewport.height) {
        y = (f32::from(anchor.y) - POPOVER_H - 4.0).max(0.0);
    }

    div()
        .id(overlay_id.clone())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        // Only capture clicks while open so the overlay doesn't block
        // the app while animating out.
        .when(open, |el| {
            el.occlude().on_click(move |_e, _w, cx| {
                let _ = entity.update(cx, |this, cx| this.hide(cx));
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            open,
            DURATION_BASE,
            EASE_STANDARD,
            |el| el.bg(rgba(0x00000000)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(
            div()
                .id(popover_id.clone())
                .absolute()
                .top(px(y))
                .left(px(x))
                .w(px(POPOVER_W))
                .h(px(POPOVER_H))
                .flex()
                .flex_col()
                .bg(rgb(bg_base()))
                .border_1()
                .border_color(rgb(border()))
                .rounded(RADIUS_MD)
                .shadow_lg()
                .overflow_hidden()
                .opacity(0.0)
                .mt(px(-4.0))
                .with_transition(popover_id)
                .transition_when_else(
                    open,
                    DURATION_BASE,
                    EASE_STANDARD,
                    |el| el.opacity(1.0).mt_0(),
                    |el| el.opacity(0.0).mt(px(-4.0)),
                )
                // Stop clicks on the popover from bubbling to the
                // backdrop (which would dismiss it).
                .when(open, |el| {
                    el.on_click(|_e, _w, cx| {
                        cx.stop_propagation();
                    })
                })
                .child(render_header(records.len()))
                .child(render_list(records)),
        )
}

fn render_header(count: usize) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_1p5()
        .border_b_1()
        .border_color(rgb(border()))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(text_primary()))
                .child(t!("sftp.history_title").to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(format!("{}", count)),
        )
}

fn render_list(records: Vec<TransferRecord>) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .overflow_y_scrollbar()
        .when(records.is_empty(), |el| {
            el.child(
                div()
                    .h(px(80.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(t!("sftp.history_empty").to_string()),
            )
        })
        .children(
            records
                .into_iter()
                .enumerate()
                .map(|(i, r)| render_history_row(i, r)),
        )
}

fn render_history_row(idx: usize, record: TransferRecord) -> impl IntoElement {
    let (icon_path, kind_color) = match record.kind {
        SftpTransferKind::Download => {
            ("icons/terminal-toolbar/arrow-down-to-line.svg", term_blue())
        }
        SftpTransferKind::Upload => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_green()),
        SftpTransferKind::Rename => ("icons/terminal-toolbar/edit.svg", term_yellow()),
        SftpTransferKind::Edit => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_green()),
        SftpTransferKind::Delete => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_red()),
    };
    let status_color = if record.success {
        term_green()
    } else {
        term_red()
    };
    let status_label = if record.success {
        t!("sftp.history_status_success").to_string()
    } else {
        t!("sftp.history_status_failed").to_string()
    };
    let message = crate::layouts::toolbar::truncate_path_middle(&record.message, 40);
    let time_label = format_local_time(record.finished_at);

    div()
        .id(ElementId::Name(format!("history-row-{}", idx).into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .border_b_1()
        .border_color(rgb(border()))
        .child(
            svg()
                .path(icon_path)
                .size(px(12.0))
                .flex_shrink_0()
                .text_color(rgb(kind_color)),
        )
        .child(
            div()
                .text_xs()
                .min_w(px(50.0))
                .text_color(rgb(kind_color))
                .child(kind_label(record.kind)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .min_w_0()
                .flex_1()
                .truncate()
                .child(message),
        )
        .child(
            div()
                .text_xs()
                .flex_shrink_0()
                .text_color(rgb(text_muted()))
                .child(time_label),
        )
        .child(
            div()
                .text_xs()
                .flex_shrink_0()
                .text_color(rgb(status_color))
                .child(status_label),
        )
}

fn kind_label(kind: SftpTransferKind) -> String {
    match kind {
        SftpTransferKind::Download => t!("sftp.progress.download").to_string(),
        SftpTransferKind::Upload => t!("sftp.progress.upload").to_string(),
        SftpTransferKind::Rename => t!("sftp.rename").to_string(),
        SftpTransferKind::Edit => t!("sftp.progress.upload").to_string(),
        SftpTransferKind::Delete => t!("sftp.delete").to_string(),
    }
}

/// Format a Unix timestamp as a short local-time string (HH:MM:SS).
/// Uses `chrono` so the user's timezone + DST are respected.
fn format_local_time(secs: i64) -> String {
    use chrono::TimeZone;
    if secs <= 0 {
        return "--:--:--".to_string();
    }
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%H:%M:%S").to_string(),
        _ => "--:--:--".to_string(),
    }
}
