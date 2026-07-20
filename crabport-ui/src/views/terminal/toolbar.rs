//! Concrete bottom-toolbar rendering for the **terminal** tab.
//!
//! This module is the bridge between the generic toolbar framework
//! ([`crate::layouts::toolbar`]) and the terminal-tab's data: it knows how
//! to turn a [`TerminalToolbarInput`] (status + metrics + SFTP progress)
//! into a list of [`ToolbarSlot`]s, each of which renders one chip.
//!
//! The framework handles layout, the open/close height animation, and the
//! gear-button context menu that toggles each slot's visibility. The
//! persistence of the toggle state lives in
//! `[appearance.terminal.toolbar]` (see `crabport_core::config`).
//!
//! Slots, in left-to-right order:
//!   1. `latency`  — connection dot + RTT
//!   2. `memory`   — used/total bar
//!   3. `cpu`      — usage % (and load avg on Unix)
//!   4. `disk`     — used/total bar for the primary disk
//!   5. `network`  — ↑/↓ rates
//!   6. `sftp_progress` (right-aligned) — in-flight SFTP transfer chip

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crabport_terminal::terminal::{
    CpuStats, DiskStats, MemoryStats, NetworkStats, RemoteMetrics, RemoteStatus, SftpTransferBytes,
    SftpTransferKind, SftpTransferStage,
};
use rust_i18n::t;

use crate::color::*;
use crate::components::context_menu::ContextMenuController;
use crate::layouts::toolbar::{
    BAR_HEIGHT, BAR_WIDTH, ToolbarProps, ToolbarSlot, color_accent, format_byte_ratio,
    format_memory, format_rate, human_bytes, status_color, truncate_path_middle,
};
use crate::motion::{duration_slower, EASE_STANDARD};
use crate::views::terminal::SftpProgress;

// ---------------------------------------------------------------------------
// TerminalToolbarInput — what the caller (content.rs) passes in
// ---------------------------------------------------------------------------

/// Everything the terminal toolbar needs to render one frame.
///
/// `show_metrics` mirrors the old `render_terminal_toolbar` flag — false
/// for the SFTP tab (which has no monitor of its own), true for terminal
/// tabs. SFTP tabs still pass an `sftp_progress` value and let the
/// framework render just that chip.
#[derive(Clone)]
pub struct TerminalToolbarInput {
    pub show_metrics: bool,
    pub status: RemoteStatus,
    pub metrics: RemoteMetrics,
    pub sftp_progress: Option<SftpProgress>,
}

impl Default for TerminalToolbarInput {
    fn default() -> Self {
        Self {
            show_metrics: false,
            status: RemoteStatus::Local,
            metrics: RemoteMetrics::default(),
            sftp_progress: None,
        }
    }
}

impl TerminalToolbarInput {
    pub fn new(
        show_metrics: bool,
        status: RemoteStatus,
        metrics: RemoteMetrics,
        sftp_progress: Option<SftpProgress>,
    ) -> Self {
        Self {
            show_metrics,
            status,
            metrics,
            sftp_progress,
        }
    }
}

// ---------------------------------------------------------------------------
// render_terminal_toolbar — entry point called from content.rs
// ---------------------------------------------------------------------------

/// Build the terminal-tab toolbar.
///
/// `on_toggle` flips the visibility of a slot (by id) and is responsible
/// for persisting the change. `context_menu` is the global controller used
/// to show the gear menu; pass `None` to hide the gear entirely.
///
/// `trailing` is a list of extra right-aligned elements rendered *before*
/// the gear button. Callers that don't have toolbar-internal actions
/// (toggles, status chips that aren't slot-controlled) should pass an
/// empty vec. The SFTP tab uses this to inject its "transfer history"
/// toggle button without the terminal toolbar knowing about SFTP.
pub fn render_terminal_toolbar(
    input: TerminalToolbarInput,
    context_menu: Option<Entity<ContextMenuController>>,
    trailing: Vec<AnyElement>,
    on_toggle: impl Fn(&str, &mut App) + 'static,
) -> impl IntoElement {
    // Read the current per-slot visibility from config so the ctxmenu
    // checkmarks match what's actually shown. We snapshot it here (cheap
    // clone of a small struct) so the closures below don't have to re-read
    // config on every frame.
    let vis = crabport_core::config::snapshot()
        .appearance
        .terminal
        .toolbar
        .clone();

    let mut props = ToolbarProps::new(on_toggle);
    if let Some(cm) = context_menu {
        props = props.context_menu(cm);
    }
    for el in trailing {
        props = props.trailing(el);
    }

    // Order matters — this is the left-to-right display order.
    props = props.slot(latency_slot(
        input.show_metrics,
        input.status,
        input.metrics.latency_ms,
        vis.latency,
    ));
    props = props.slot(cpu_slot(input.show_metrics, input.metrics.cpu, vis.cpu));
    props = props.slot(memory_slot(
        input.show_metrics,
        input.metrics.memory,
        vis.memory,
    ));
    props = props.slot(disk_slot(input.show_metrics, input.metrics.disk, vis.disk));
    props = props.slot(network_slot(
        input.show_metrics,
        input.metrics.network,
        vis.network,
    ));
    props = props.slot(sftp_progress_slot(
        input.sftp_progress.clone(),
        vis.sftp_progress,
    ));

    crate::layouts::toolbar::render_toolbar(props)
}

// ---------------------------------------------------------------------------
// Latency slot
// ---------------------------------------------------------------------------

fn latency_slot(
    show_metrics: bool,
    status: RemoteStatus,
    latency_ms: Option<u32>,
    visible: bool,
) -> ToolbarSlot {
    ToolbarSlot::new(
        "latency",
        t!("toolbar.latency").to_string(),
        visible,
        move || {
            if !show_metrics {
                return None;
            }
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .min_w(px(50.0))
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(rgb(status_color(status))),
                    )
                    .child(div().text_xs().child(match latency_ms {
                        Some(ms) => format!("{}ms", ms),
                        None => "—".into(),
                    }))
                    .into_any_element(),
            )
        },
    )
}

// ---------------------------------------------------------------------------
// Memory slot
// ---------------------------------------------------------------------------

fn memory_slot(show_metrics: bool, memory: Option<MemoryStats>, visible: bool) -> ToolbarSlot {
    ToolbarSlot::new(
        "memory",
        t!("toolbar.memory").to_string(),
        visible,
        move || {
            if !show_metrics {
                return None;
            }
            let mem = memory?;
            if mem.total == 0 {
                return None;
            }
            let ratio = (mem.used as f64 / mem.total as f64).clamp(0.0, 1.0);
            let filled_w = BAR_WIDTH * ratio as f32;
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .min_w(px(180.0))
                    .child(
                        svg()
                            .path("icons/terminal-toolbar/memory-stick.svg")
                            .size(px(14.0))
                            .text_color(rgb(text_muted())),
                    )
                    // Progress bar
                    .child(
                        div()
                            .w(px(BAR_WIDTH))
                            .h(px(BAR_HEIGHT))
                            .rounded(px(3.0))
                            .bg(rgb(border()))
                            .child(
                                div()
                                    .id("memory-bar-fill")
                                    .h_full()
                                    .rounded(px(3.0))
                                    .bg(rgb(color_accent()))
                                    .with_transition("memory-bar-fill")
                                    .transition_when(
                                        true,
                                        duration_slower(),
                                        EASE_STANDARD,
                                        move |el| el.w(px(filled_w)),
                                    ),
                            ),
                    )
                    .child(div().text_xs().child(format_memory(mem.used, mem.total)))
                    .into_any_element(),
            )
        },
    )
}

// ---------------------------------------------------------------------------
// CPU slot — NEW
// ---------------------------------------------------------------------------

fn cpu_slot(show_metrics: bool, cpu: Option<CpuStats>, visible: bool) -> ToolbarSlot {
    ToolbarSlot::new("cpu", t!("toolbar.cpu").to_string(), visible, move || {
        if !show_metrics {
            return None;
        }
        let cpu = cpu?;
        // Render the percentage as a small bar + a numeric label.
        let ratio = (cpu.usage_pct / 100.0).clamp(0.0, 1.0);
        let filled_w = BAR_WIDTH * ratio as f32;
        let label = format!("{:.0}%", cpu.usage_pct);
        // Pick a fill color based on usage: green under 60%, yellow
        // 60–85%, red above. This gives a quick visual cue when a
        // box is under heavy load without requiring the user to read
        // the number.
        let fill_color = if cpu.usage_pct >= 85.0 {
            term_red()
        } else if cpu.usage_pct >= 60.0 {
            term_yellow()
        } else {
            color_accent()
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .min_w(px(100.0))
                .child(
                    svg()
                        .path("icons/cpu.svg")
                        .size(px(14.0))
                        .text_color(rgb(text_muted())),
                )
                .child(
                    div()
                        .w(px(BAR_WIDTH))
                        .h(px(BAR_HEIGHT))
                        .rounded(px(3.0))
                        .bg(rgb(border()))
                        .child(
                            div()
                                .id("cpu-bar-fill")
                                .h_full()
                                .rounded(px(3.0))
                                .bg(rgb(fill_color))
                                .with_transition("cpu-bar-fill")
                                .transition_when(true, duration_slower(), EASE_STANDARD, move |el| {
                                    el.w(px(filled_w))
                                }),
                        ),
                )
                .child(div().text_xs().child(label))
                .into_any_element(),
        )
    })
}

// ---------------------------------------------------------------------------
// Disk slot — NEW
// ---------------------------------------------------------------------------

fn disk_slot(show_metrics: bool, disk: Option<DiskStats>, visible: bool) -> ToolbarSlot {
    ToolbarSlot::new("disk", t!("toolbar.disk").to_string(), visible, move || {
        if !show_metrics {
            return None;
        }
        let disk = disk?;
        if disk.total == 0 {
            return None;
        }
        let ratio = (disk.used as f64 / disk.total as f64).clamp(0.0, 1.0);
        let filled_w = BAR_WIDTH * ratio as f32;
        // Same color bands as CPU: green under 70%, yellow 70–90%,
        // red above. Disk pressure tends to be a stronger signal than
        // CPU pressure, so the thresholds are a bit higher.
        let fill_color = if ratio >= 0.90 {
            term_red()
        } else if ratio >= 0.70 {
            term_yellow()
        } else {
            color_accent()
        };
        let (used_val, used_unit) = human_bytes(disk.used);
        let (total_val, total_unit) = human_bytes(disk.total);
        let label = format!(
            "{:.1}{} / {:.1}{}",
            used_val, used_unit, total_val, total_unit
        );
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .min_w(px(180.0))
                .child(
                    svg()
                        .path("icons/hard-drive.svg")
                        .size(px(14.0))
                        .text_color(rgb(text_muted())),
                )
                .child(
                    div()
                        .w(px(BAR_WIDTH))
                        .h(px(BAR_HEIGHT))
                        .rounded(px(3.0))
                        .bg(rgb(border()))
                        .child(
                            div()
                                .id("disk-bar-fill")
                                .h_full()
                                .rounded(px(3.0))
                                .bg(rgb(fill_color))
                                .with_transition("disk-bar-fill")
                                .transition_when(true, duration_slower(), EASE_STANDARD, move |el| {
                                    el.w(px(filled_w))
                                }),
                        ),
                )
                .child(div().text_xs().child(label))
                .into_any_element(),
        )
    })
}

// ---------------------------------------------------------------------------
// Network slot
// ---------------------------------------------------------------------------

fn network_slot(show_metrics: bool, network: Option<NetworkStats>, visible: bool) -> ToolbarSlot {
    ToolbarSlot::new(
        "network",
        t!("toolbar.network").to_string(),
        visible,
        move || {
            if !show_metrics {
                return None;
            }
            let net = network?;
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .min_w(px(180.0))
                    // Upload
                    .child(
                        svg()
                            .path("icons/terminal-toolbar/arrow-up-to-line.svg")
                            .size(px(12.0))
                            .text_color(rgb(text_muted())),
                    )
                    .child(div().text_xs().child(format_rate(net.bytes_sent)))
                    // Download
                    .child(
                        svg()
                            .path("icons/terminal-toolbar/arrow-down-to-line.svg")
                            .size(px(12.0))
                            .text_color(rgb(text_muted())),
                    )
                    .child(div().text_xs().child(format_rate(net.bytes_recv)))
                    .into_any_element(),
            )
        },
    )
}

// ---------------------------------------------------------------------------
// SFTP progress slot
// ---------------------------------------------------------------------------

/// Common display data computed from an [`SftpProgress`] snapshot.
struct SftpProgressDisplay {
    kind_label: String,
    stage_label: String,
    stage_color: u32,
    icon_path: &'static str,
    detail: String,
}

impl SftpProgressDisplay {
    fn new(p: &SftpProgress) -> Self {
        let kind_label = match p.kind {
            SftpTransferKind::Download => t!("sftp.progress.download").to_string(),
            SftpTransferKind::Upload => t!("sftp.progress.upload").to_string(),
            SftpTransferKind::Rename => t!("sftp.rename").to_string(),
            SftpTransferKind::Edit => t!("sftp.progress.upload").to_string(),
            SftpTransferKind::Delete => t!("sftp.delete").to_string(),
        };
        let (stage_label, stage_color) = match p.stage {
            SftpTransferStage::Compress => {
                (t!("sftp.progress.compress").to_string(), term_yellow())
            }
            SftpTransferStage::Transfer => {
                (t!("sftp.progress.transfer").to_string(), color_accent())
            }
            SftpTransferStage::Decompress => {
                (t!("sftp.progress.decompress").to_string(), term_yellow())
            }
            SftpTransferStage::CleanUp => (t!("sftp.progress.cleanup").to_string(), text_muted()),
        };
        let icon_path = match p.kind {
            SftpTransferKind::Download => "icons/terminal-toolbar/arrow-down-to-line.svg",
            SftpTransferKind::Upload => "icons/terminal-toolbar/arrow-up-to-line.svg",
            SftpTransferKind::Rename => "icons/terminal-toolbar/edit.svg",
            SftpTransferKind::Edit => "icons/terminal-toolbar/arrow-up-to-line.svg",
            SftpTransferKind::Delete => "icons/terminal-toolbar/arrow-up-to-line.svg",
        };
        let detail = truncate_path_middle(&p.message, 40);
        Self {
            kind_label,
            stage_label,
            stage_color,
            icon_path,
            detail,
        }
    }
}

fn sftp_progress_slot(progress: Option<SftpProgress>, visible: bool) -> ToolbarSlot {
    ToolbarSlot::new(
        "sftp_progress",
        t!("toolbar.sftp_progress").to_string(),
        visible,
        move || {
            let p = progress.clone()?;
            let d = SftpProgressDisplay::new(&p);
            let bar = render_progress_bar(p.bytes, d.stage_color, "sftp-progress-bar-fill");
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .child(
                        svg()
                            .path(d.icon_path)
                            .size(px(12.0))
                            .flex_shrink_0()
                            .text_color(rgb(d.stage_color)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(d.stage_color))
                            .child(d.stage_label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .min_w_0()
                            .truncate()
                            .child(format!("{}: {}", d.kind_label, d.detail)),
                    )
                    .when_some(bar, |el, bar| {
                        el.child(bar).when_some(p.bytes, |el, b| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .flex_shrink_0()
                                    .child(format_byte_ratio(b.done, b.total)),
                            )
                        })
                    })
                    .into_any_element(),
            )
        },
    )
}

/// Render a thin determinate progress bar when byte counts are available.
/// Returns `None` for indeterminate stages (no `bytes` field).
fn render_progress_bar(
    bytes: Option<SftpTransferBytes>,
    color: u32,
    fill_id: &'static str,
) -> Option<impl IntoElement> {
    let b = bytes?;
    let ratio = if b.total == 0 {
        0.0
    } else {
        (b.done as f64 / b.total as f64).clamp(0.0, 1.0)
    };
    let filled_w = BAR_WIDTH * ratio as f32;
    Some(
        div()
            .w(px(BAR_WIDTH))
            .h(px(BAR_HEIGHT))
            .rounded(px(3.0))
            .bg(rgb(border()))
            .flex_shrink_0()
            .child(
                div()
                    .id(fill_id)
                    .h_full()
                    .rounded(px(3.0))
                    .bg(rgb(color))
                    .with_transition(fill_id)
                    .transition_when(true, duration_slower(), EASE_STANDARD, move |el| {
                        el.w(px(filled_w))
                    }),
            ),
    )
}
