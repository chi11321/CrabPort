//! SFTP transfer panel — live jobs (with pause / cancel) plus recent
//! history.
//!
//! Like the global tooltip / context-menu controllers, this is an `Entity`
//! held by the app root and rendered as a top-level overlay (not inside the
//! `SftpTabView` view tree). It opens from either of the SFTP tab's toolbar
//! affordances:
//!
//! - the **transfer-history** button, and
//! - the **in-flight progress chip** (clicking it expands the same panel).
//!
//! ## Jobs vs. records
//!
//! A [`TransferJob`] is one transfer the user started from a UI surface that
//! went through this controller. The job owns the transfer's
//! [`TransferControl`], so the panel can
//!
//! - render live byte progress (the backend mirrors `done` / `total` and the
//!   current stage into the control on every chunk), and
//! - pause / resume / cancel it from the row's buttons.
//!
//! When the backend reports the transfer finished it calls
//! [`TransferControl::finish`]. The next render retires the job into
//! [`TransferRecord`] history (see [`TransferCenter::retire_finished`]) —
//! that is what keeps the panel honest even if it missed the broadcast
//! event. Non-transfer operations (rename / delete / mkdir / edit) push
//! records directly via [`TransferCenter::push`].

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;

use crate::color::*;
use crate::layouts::toolbar::color_accent;
use crate::motion::{EASE_STANDARD, RADIUS_LG, RADIUS_SM, duration_base, duration_fast};
use crabport_sftp::TransferControl;
use crabport_terminal::terminal::SftpTransferKind;

/// Maximum history records kept in memory. Older entries are dropped (FIFO).
const MAX_RECORDS: usize = 200;
/// Hard cap on simultaneous job rows — a runaway caller can't grow the panel
/// without bound. Transfers past the cap still run; they just aren't listed.
const MAX_JOBS: usize = 32;
/// Pop-in/out animation duration. Must match the `transition_when_else`
/// duration in `render_panel` so the dismiss timer clears state at the right
/// moment.
const DISMISS_MS: u64 = 180;

/// Panel width in px. Wide enough for a two-line job row with two trailing
/// buttons, narrow enough to read as a popover anchored to the toolbar.
const PANEL_W: f32 = 420.0;
/// Panel height in px. Fixed so the popover doesn't jump as jobs start and
/// finish; the list inside scrolls instead.
const PANEL_H: f32 = 380.0;

/// Row heights. The list is virtualized (`v_virtual_list`), which needs a
/// height per item up front; active jobs are two-line rows with buttons,
/// history rows are single-line.
const JOB_ROW_HEIGHT: f32 = 52.0;
const RECORD_ROW_HEIGHT: f32 = 30.0;

/// One row of the merged transfer list: an in-flight job or a finished
/// record. Both share one virtualized scroll area so the history stays
/// visible while a transfer runs.
#[derive(Clone)]
enum TransferRow {
    Job(TransferJob),
    Record(TransferRecord),
}

/// A completed SFTP transfer, captured for the history section.
///
/// Records are kept in-memory only (one `Vec` per controller), most-
/// recent-first. We don't persist them to the Store — the history is meant
/// as a "what just happened" aid within a session, not a permanent log.
#[derive(Clone, Debug)]
pub struct TransferRecord {
    pub kind: SftpTransferKind,
    pub success: bool,
    /// The backend-supplied detail string — typically the path of the file
    /// that was processed. Kept verbatim so the user sees exactly what the
    /// backend reported.
    pub message: String,
    /// When the transfer finished (local Unix timestamp), for display.
    pub finished_at: i64,
}

/// One in-flight transfer the user can pause / cancel.
#[derive(Clone)]
pub struct TransferJob {
    /// Monotonic id, unique within this controller.
    pub id: u64,
    pub kind: SftpTransferKind,
    /// Short, human-readable subject: the file name (or "N files" for a
    /// batch), not the full path — the panel is narrow.
    pub label: String,
    /// The transfer's control handle. Shared with the backend task.
    pub control: Arc<TransferControl>,
    /// When the job started (local Unix timestamp), for display.
    pub started_at: i64,
}

/// Global SFTP transfer panel state.
pub struct TransferCenter {
    /// Most-recent-first. Capped at [`MAX_RECORDS`].
    records: Vec<TransferRecord>,
    /// In-flight jobs, most-recent-first.
    jobs: Vec<TransferJob>,
    /// Scroll position for the virtualized list. Persisted across renders so
    /// reopening the panel lands where the user left off.
    scroll_handle: VirtualListScrollHandle,
    /// Where to anchor the panel (button's window-relative position).
    anchor: Option<Point<Pixels>>,
    open: bool,
    /// Monotonic counter bumped on every show/hide so stale dismiss tasks
    /// don't clobber a freshly-shown panel.
    generation: u64,
    /// Id source for [`TransferJob`]s.
    next_id: u64,
}

impl TransferCenter {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            jobs: Vec::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            anchor: None,
            open: false,
            generation: 0,
            next_id: 1,
        }
    }

    /// Append a history record (for operations that don't run as jobs —
    /// rename / delete / mkdir / edit, or transfers started outside the UI
    /// surfaces that create jobs).
    pub fn push(&mut self, record: TransferRecord, cx: &mut Context<Self>) {
        self.records.insert(0, record);
        if self.records.len() > MAX_RECORDS {
            self.records.truncate(MAX_RECORDS);
        }
        cx.notify();
    }

    /// Register a job and hand back its id. Called *before* the backend
    /// transfer starts so the panel never shows a job that already completed.
    pub fn start_job(
        &mut self,
        kind: SftpTransferKind,
        label: impl Into<String>,
        control: Arc<TransferControl>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.jobs.len() < MAX_JOBS {
            self.jobs.insert(
                0,
                TransferJob {
                    id,
                    kind,
                    label: label.into(),
                    control,
                    started_at: now_secs(),
                },
            );
        }
        cx.notify();
        id
    }

    /// Reap jobs whose transfer has finished and file them as history.
    ///
    /// Safe to call every render: the work is proportional to the number of
    /// active jobs (a handful) and no-ops when nothing finished. We
    /// deliberately do **not** notify from here — the caller is already
    /// inside a render.
    fn retire_finished(&mut self) {
        let mut kept: Vec<TransferJob> = Vec::with_capacity(self.jobs.len());
        for job in self.jobs.drain(..) {
            if !job.control.is_finished() {
                kept.push(job);
                continue;
            }
            // A cancelled job reports `success == false`; use a dedicated
            // "Cancelled" label rather than the backend's echoed error text
            // (which reads like a failure the user didn't ask for). Successful
            // jobs keep their file name so the row doesn't change wording the
            // moment it slides from the active block into history; failures
            // keep the backend's error text because that is the useful part.
            let message = if job.control.is_cancelled() {
                t!("sftp.transfer_cancelled").to_string()
            } else if job.control.success() {
                job.label.clone()
            } else {
                job.control.message()
            };
            self.records.insert(
                0,
                TransferRecord {
                    kind: job.kind,
                    success: job.control.success(),
                    message,
                    finished_at: now_secs(),
                },
            );
        }
        self.jobs = kept;
        if self.records.len() > MAX_RECORDS {
            self.records.truncate(MAX_RECORDS);
        }
    }

    /// Toggle the panel open/closed at `anchor` (window-relative).
    pub fn toggle(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        if self.open {
            self.hide(cx);
        } else {
            self.show(anchor, cx);
        }
    }

    /// Show the panel at `anchor`, replacing any currently-showing one.
    pub fn show(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.anchor = Some(anchor);
        self.open = true;
        gpui_animation::reset_transition(&ElementId::Name("sftp-transfer-panel".into()));
        cx.notify();
    }

    /// Begin the dismiss animation. After [`DISMISS_MS`] the anchor is
    /// dropped so the panel no longer captures renders.
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

    /// Number of records currently held.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Number of transfers currently running or paused.
    pub fn active_count(&self) -> usize {
        self.jobs.len()
    }
}

impl Render for TransferCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Retire completed jobs before the anchor check so a finished job is
        // filed into history even while the panel is closed.
        self.retire_finished();

        let Some(anchor) = self.anchor else {
            return div().into_any_element();
        };
        let open = self.open;
        let viewport = _window.viewport_size();
        let entity = cx.entity().downgrade();
        let active = self.jobs.len();

        // Snapshot the merged row list (active jobs first, then history) plus
        // the per-item heights the virtual list needs. Copying handles here
        // keeps the `Fn` render closure independent of `self`.
        let mut rows: Vec<TransferRow> = Vec::with_capacity(self.jobs.len() + self.records.len());
        let mut sizes: Vec<Size<Pixels>> = Vec::with_capacity(self.jobs.len() + self.records.len());
        for job in &self.jobs {
            rows.push(TransferRow::Job(job.clone()));
            sizes.push(Size {
                width: px(0.0),
                height: px(JOB_ROW_HEIGHT),
            });
        }
        for record in &self.records {
            rows.push(TransferRow::Record(record.clone()));
            sizes.push(Size {
                width: px(0.0),
                height: px(RECORD_ROW_HEIGHT),
            });
        }
        let rows = Rc::new(rows);

        let body: AnyElement = if rows.is_empty() {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("sftp.history_empty").to_string())
                .into_any_element()
        } else {
            let scroll_handle = self.scroll_handle.clone();
            let list = v_virtual_list(cx.entity(), "sftp-transfer-list", Rc::new(sizes), {
                let rows = rows.clone();
                move |_this, range, _window, cx| {
                    let entity = cx.entity().downgrade();
                    range
                        .map(|i| match &rows[i] {
                            TransferRow::Job(job) => render_job_row(job.clone(), entity.clone()),
                            TransferRow::Record(record) => {
                                render_history_row(i, record.clone()).into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                }
            })
            .track_scroll(&scroll_handle);
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(10.0))
                        .child(
                            Scrollbar::vertical(&scroll_handle)
                                .scrollbar_show(ScrollbarShow::Hover),
                        ),
                )
                .into_any_element()
        };

        render_panel(anchor, open, body, active, viewport, entity).into_any_element()
    }
}

/// Render the panel. Anchored to `anchor` (the trigger's position), clamped
/// to stay inside the window.
fn render_panel(
    anchor: Point<Pixels>,
    open: bool,
    body: AnyElement,
    active: usize,
    viewport: Size<Pixels>,
    entity: WeakEntity<TransferCenter>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("sftp-transfer-overlay".into());
    let panel_id = ElementId::Name("sftp-transfer-panel".into());

    let mut x = f32::from(anchor.x);
    let mut y = f32::from(anchor.y) + 28.0; // below the trigger
    // Flip left when the panel would overflow the right edge.
    if x + PANEL_W > f32::from(viewport.width) {
        x = (f32::from(anchor.x) - PANEL_W).max(0.0);
    }
    // Flip up when it would overflow the bottom edge.
    if y + PANEL_H > f32::from(viewport.height) {
        y = (f32::from(anchor.y) - PANEL_H - 4.0).max(0.0);
    }

    div()
        .id(overlay_id.clone())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .when(open, {
            let entity = entity.clone();
            move |el| {
                el.occlude().on_click(move |_e, _w, cx| {
                    let _ = entity.update(cx, |this, cx| this.hide(cx));
                })
            }
        })
        .with_transition(overlay_id)
        .transition_when_else(
            open,
            duration_base(),
            EASE_STANDARD,
            |el| el.bg(rgba(0x00000000)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(
            div()
                .id(panel_id.clone())
                .absolute()
                .top(px(y))
                .left(px(x))
                .w(px(PANEL_W))
                .h(px(PANEL_H))
                .flex()
                .flex_col()
                .bg(rgb(bg_base()))
                .border_1()
                .border_color(rgb(border()))
                .rounded(RADIUS_LG)
                .shadow_lg()
                .overflow_hidden()
                .opacity(0.0)
                .mt(px(-4.0))
                .with_transition(panel_id)
                .transition_when_else(
                    open,
                    duration_base(),
                    EASE_STANDARD,
                    |el| el.opacity(1.0).mt_0(),
                    |el| el.opacity(0.0).mt(px(-4.0)),
                )
                .when(open, |el| {
                    el.on_click(|_e, _w, cx| {
                        cx.stop_propagation();
                    })
                })
                .child(render_header(active, entity.clone()))
                // One virtualized list, not two sections: in-flight jobs sit on
                // top of the recent records so a transfer stays visible next to
                // the history it is about to join.
                .child(body),
        )
}

fn render_header(active: usize, entity: WeakEntity<TransferCenter>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(border()))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(text_primary()))
                        .child(t!("sftp.transfers_title").to_string()),
                )
                .when(active > 0, |el| {
                    el.child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(rgba((color_accent() << 8) | 0x33))
                            .text_xs()
                            .text_color(rgb(color_accent()))
                            .child(active.to_string()),
                    )
                }),
        )
        .child(
            div()
                .id("sftp-transfer-close")
                .flex()
                .items_center()
                .justify_center()
                .size(px(20.0))
                .rounded(RADIUS_SM)
                .cursor_pointer()
                .on_click(move |_e, _w, cx| {
                    let _ = entity.update(cx, |this, cx| this.hide(cx));
                })
                .child(
                    svg()
                        .path("icons/close.svg")
                        .size(px(12.0))
                        .text_color(rgb(text_muted())),
                ),
        )
}

/// One active transfer: icon + label + state/progress on the left, pause and
/// cancel buttons on the right, and the progress fill riding on the row's
/// bottom rule.
fn render_job_row(job: TransferJob, entity: WeakEntity<TransferCenter>) -> AnyElement {
    let (icon_path, kind_color) = kind_icon(job.kind);
    let paused = job.control.is_paused();
    let cancelled = job.control.is_cancelled();
    let (done, total) = job.control.progress();
    let ratio = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };

    // What the row says under the file name. Cancelled wins over paused so a
    // paused-then-cancelled job reads as "cancelling", not "paused".
    let state_label = if cancelled {
        t!("sftp.transfer_cancelling").to_string()
    } else if paused {
        t!("sftp.transfer_paused").to_string()
    } else {
        stage_label(job.control.stage())
    };
    let percent_label = if total > 0 {
        format!(
            "{} · {}%",
            crate::layouts::toolbar::format_byte_ratio(done, total),
            (ratio * 100.0).round() as u32
        )
    } else {
        String::new()
    };

    div()
        .relative()
        .h(px(JOB_ROW_HEIGHT))
        .px_3()
        .border_b_1()
        .border_color(rgb(border()))
        .flex()
        .flex_col()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(icon_path)
                        .size(px(14.0))
                        .flex_shrink_0()
                        .text_color(rgb(kind_color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_primary()))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(job.label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(if paused { term_yellow() } else { text_muted() }))
                                .child(if percent_label.is_empty() {
                                    state_label
                                } else {
                                    format!("{state_label} · {percent_label}")
                                }),
                        ),
                )
                // Pause / resume — hidden once cancelled (the transfer is
                // already on its way out).
                .when(!cancelled, |el| {
                    let control = job.control.clone();
                    let entity = entity.clone();
                    el.child(action_button(
                        format!("sftp-job-pause-{}", job.id),
                        if paused {
                            "icons/play.svg"
                        } else {
                            "icons/pause.svg"
                        },
                        if paused { term_yellow() } else { text_muted() },
                        move |_e, _w, cx| {
                            control.toggle_pause();
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        },
                    ))
                })
                .when(!cancelled, |el| {
                    let control = job.control.clone();
                    let entity = entity.clone();
                    el.child(action_button(
                        format!("sftp-job-cancel-{}", job.id),
                        "icons/close.svg",
                        text_muted(),
                        move |_e, _w, cx| {
                            control.cancel();
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        },
                    ))
                }),
        )
        // Progress rides *on* the row's bottom rule: a 2px fill that grows
        // from the left edge, flush with the row's full width. The 1px
        // `border_b` underneath acts as the empty track, so there is no
        // separate bar geometry to align.
        .child(
            div()
                .absolute()
                .left_0()
                // `-1px` so the 2px fill sits *on* the 1px bottom rule
                // (0 would place it just above it, leaving the muted line
                // visible underneath).
                .bottom(px(-1.0))
                .h(px(2.0))
                .w(DefiniteLength::Fraction(ratio as f32))
                .bg(rgb(if cancelled {
                    term_red()
                } else if paused {
                    term_yellow()
                } else {
                    kind_color
                })),
        )
        .into_any_element()
}

/// Small ghost icon button used by job rows. Sized for a popover (18px), not
/// the 24px toolbar variant.
fn action_button(
    id: String,
    icon: &'static str,
    color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let btn_id = ElementId::Name(id.into());
    div()
        .id(btn_id.clone())
        .flex()
        .items_center()
        .justify_center()
        .size(px(18.0))
        .flex_shrink_0()
        .rounded(RADIUS_SM)
        .cursor_pointer()
        .bg(rgba(0x00000000))
        .with_transition(btn_id)
        .transition_on_hover(duration_fast(), EASE_STANDARD, |hovered, el| {
            if *hovered {
                el.bg(rgb(surface_hover()))
            } else {
                el.bg(rgba(0x00000000))
            }
        })
        .on_click(on_click)
        .child(svg().path(icon).size(px(12.0)).text_color(rgb(color)))
        .into_any_element()
}

fn render_history_row(idx: usize, record: TransferRecord) -> impl IntoElement {
    let (icon_path, kind_color) = kind_icon(record.kind);
    // Status is conveyed by icon + color only, so a mixed-locale / narrow
    // panel stays readable and the row doesn't spend width on a word.
    let (status_icon, status_color) = if record.success {
        ("icons/check.svg", term_green())
    } else {
        ("icons/close.svg", term_red())
    };
    let message = crate::layouts::toolbar::truncate_path_middle(&record.message, 44);
    let time_label = format_local_time(record.finished_at);

    div()
        .id(ElementId::Name(format!("history-row-{}", idx).into()))
        .h(px(RECORD_ROW_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(rgb(border()))
        // Leading affordance: direction icon only (no "Download" / "Upload"
        // label) so the eye lands on the file name.
        .child(
            svg()
                .path(icon_path)
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(rgb(kind_color)),
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
        // Trailing status: check / cross icon instead of "Success" / "Failed".
        .child(
            svg()
                .path(status_icon)
                .size(px(13.0))
                .flex_shrink_0()
                .text_color(rgb(status_color)),
        )
}

fn kind_icon(kind: SftpTransferKind) -> (&'static str, u32) {
    match kind {
        SftpTransferKind::Download => {
            ("icons/terminal-toolbar/arrow-down-to-line.svg", term_blue())
        }
        SftpTransferKind::Upload => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_green()),
        SftpTransferKind::Rename => ("icons/terminal-toolbar/edit.svg", term_yellow()),
        SftpTransferKind::Edit => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_green()),
        SftpTransferKind::Delete => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_red()),
        SftpTransferKind::Mkdir => ("icons/terminal-toolbar/arrow-up-to-line.svg", term_yellow()),
    }
}

/// Human-readable label for the control's stage id.
fn stage_label(stage: u8) -> String {
    match stage {
        crabport_sftp::STAGE_COMPRESS => t!("sftp.progress.compress").to_string(),
        crabport_sftp::STAGE_TRANSFER => t!("sftp.progress.transfer").to_string(),
        crabport_sftp::STAGE_DECOMPRESS => t!("sftp.progress.decompress").to_string(),
        crabport_sftp::STAGE_CLEANUP => t!("sftp.progress.cleanup").to_string(),
        // `STAGE_NONE` and anything unknown: the transfer is queued but hasn't
        // reported a stage yet.
        _ => t!("sftp.transfer_waiting").to_string(),
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Human-readable subject for a job row: the path's final component, so the
/// panel reads as a list of file *names* rather than full paths. Falls back
/// to the whole path when it has no file name (e.g. a bare `/`).
///
/// Public so the surfaces that start transfers (the SFTP tab's panel
/// callbacks and the terminal tab's right-hand SFTP panel) label jobs the
/// same way.
pub fn transfer_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
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
