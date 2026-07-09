use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutCubic};
use std::time::Duration;

use crabport_terminal::terminal::{
    MemoryStats, NetworkStats, RemoteMetrics, RemoteStatus, SftpTransferBytes, SftpTransferKind,
    SftpTransferStage,
};
use rust_i18n::t;

use crate::color::*;
use crate::views::terminal::SftpProgress;

const TOOLBAR_HEIGHT: f32 = 36.0;

// ---------------------------------------------------------------------------
// Status colors
// ---------------------------------------------------------------------------

fn status_color(status: RemoteStatus) -> u32 {
    match status {
        RemoteStatus::Local => term_bright_black(),
        RemoteStatus::Connected => term_green(),
        RemoteStatus::Connecting => term_yellow(),
        RemoteStatus::Disconnected => term_red(),
    }
}

// Progress bar dimensions
const BAR_WIDTH: f32 = 80.0;
const BAR_HEIGHT: f32 = 6.0;

// ---------------------------------------------------------------------------
// Main render
// ---------------------------------------------------------------------------

pub fn render_terminal_toolbar(
    is_terminal: bool,
    status: RemoteStatus,
    metrics: RemoteMetrics,
    sftp_progress: Option<SftpProgress>,
) -> impl IntoElement {
    // Only render the toolbar contents when metrics have actually been loaded.
    // Matches the SFTP panel pattern: no data → no element tree.
    //
    // We also keep the toolbar open while an SFTP transfer is in flight so
    // the progress log stays visible even if metrics haven't loaded yet
    // (e.g. on a freshly connected host before the first monitor tick).
    let has_metrics = is_terminal
        && (metrics.latency_ms.is_some() || metrics.memory.is_some() || metrics.network.is_some());
    let has_progress = is_terminal && sftp_progress.is_some();
    let show_toolbar = has_metrics || has_progress;

    div()
        .id("terminal-toolbar")
        .w_full()
        .overflow_hidden()
        .border_t_1()
        .with_transition("terminal-toolbar-height")
        .transition_when_else(
            show_toolbar,
            Duration::from_millis(500),
            EaseInOutCubic,
            |el| el.h(px(TOOLBAR_HEIGHT)),
            |el| el.h_0(),
        )
        .bg(rgb(bg_tab_bar()))
        .border_b_1()
        .border_color(rgb(border()))
        .when(show_toolbar, |el| {
            el.child(
                div()
                    .w_full()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .gap_4()
                    .text_color(rgb(text_muted()))
                    .child(render_connection(status, metrics.latency_ms))
                    .children(render_memory(metrics.memory))
                    .children(render_network(metrics.network))
                    // Flexible spacer pushes the SFTP progress log to the
                    // far right edge of the toolbar.
                    .child(div().flex_1())
                    .children(render_sftp_progress(sftp_progress)),
            )
        })
}

// ---------------------------------------------------------------------------
// Connection status + latency
// ---------------------------------------------------------------------------

fn render_connection(status: RemoteStatus, latency_ms: Option<u32>) -> impl IntoElement {
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
}

// ---------------------------------------------------------------------------
// Memory: progress bar + "xxxM / xxxG"
// ---------------------------------------------------------------------------

fn render_memory(memory: Option<MemoryStats>) -> Option<impl IntoElement> {
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
                                Duration::from_millis(300),
                                EaseInOutCubic,
                                move |el| el.w(px(filled_w)),
                            ),
                    ),
            )
            .child(div().text_xs().child(format_memory(mem.used, mem.total))),
    )
}

// ---------------------------------------------------------------------------
// Network: ↑/↓ icons + rate
// ---------------------------------------------------------------------------

fn render_network(network: Option<NetworkStats>) -> Option<impl IntoElement> {
    let net = network?;
    // We show cumulative totals — the caller can switch to rate if desired.
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
            .child(div().text_xs().child(format_rate(net.bytes_recv))),
    )
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format used/total as e.g. "2.1G / 16.0G" or "512.0M / 8.0G"
fn format_memory(used: u64, total: u64) -> String {
    let (used_val, used_unit) = human_bytes(used);
    let (total_val, total_unit) = human_bytes(total);
    format!(
        "{:.1}{} / {:.1}{}",
        used_val, used_unit, total_val, total_unit
    )
}

fn human_bytes(bytes: u64) -> (f64, &'static str) {
    let b = bytes as f64;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    if b >= GB {
        (b / GB, "G")
    } else if b >= MB {
        (b / MB, "M")
    } else if b >= KB {
        (b / KB, "K")
    } else {
        (b, "B")
    }
}

fn format_rate(bytes_per_sec: u64) -> String {
    let (val, unit) = human_bytes(bytes_per_sec);
    format!("{:.1}{}/s", val, unit)
}

// ---------------------------------------------------------------------------
// SFTP transfer progress log
// ---------------------------------------------------------------------------

/// Render the right-aligned SFTP progress chip.
///
/// Returns `None` when there's no in-flight transfer, so the caller's
/// `.children(...)` renders nothing.
///
/// Layout: direction icon + stage label + path, all in muted text so it
/// reads as ambient status rather than a primary control. We deliberately
/// don't animate a spinner here — the toolbar re-renders on every progress
/// event (compress → transfer → decompress → cleanup), so the changing
/// stage label itself conveys activity without the cost of a continuous
/// animation loop on the app-level render path.
fn render_sftp_progress(progress: Option<SftpProgress>) -> Option<impl IntoElement> {
    let p = progress?;
    let kind_label = match p.kind {
        SftpTransferKind::Download => t!("sftp.progress.download").to_string(),
        SftpTransferKind::Upload => t!("sftp.progress.upload").to_string(),
        SftpTransferKind::Rename => t!("sftp.rename").to_string(),
        SftpTransferKind::Edit => t!("sftp.progress.upload").to_string(),
    };
    let (stage_label, stage_color) = match p.stage {
        SftpTransferStage::Compress => (t!("sftp.progress.compress").to_string(), term_yellow()),
        SftpTransferStage::Transfer => (t!("sftp.progress.transfer").to_string(), color_accent()),
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
    };
    // Middle-truncate long paths so the head (top-level dir) and tail
    // (filename) stay visible — e.g. "/home/user/.../deeply/nested/file.txt".
    let detail = truncate_path_middle(&p.message, 40);

    // Optional determinate progress bar, shown only when the backend
    // reports byte counts (currently the Transfer stage). Indeterminate
    // stages (Compress/Decompress/CleanUp run via opaque remote exec) get
    // no bar — the changing stage label conveys activity.
    let bar = render_progress_bar(p.bytes, stage_color);

    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .child(
                svg()
                    .path(icon_path)
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(rgb(stage_color)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(stage_color))
                    .child(stage_label),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .min_w_0()
                    .truncate()
                    .child(format!("{kind_label}: {detail}")),
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
            }),
    )
}

/// Render a thin determinate progress bar when byte counts are available.
/// Returns `None` for indeterminate stages (no `bytes` field).
///
/// Uses `gpui-animation`'s `transition_when` with a stable element id so the
/// fill width eases between updates — same pattern as the memory-usage bar.
/// Without this, each progress event would snap the fill to its new width.
fn render_progress_bar(bytes: Option<SftpTransferBytes>, color: u32) -> Option<impl IntoElement> {
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
                    .id("sftp-progress-bar-fill")
                    .h_full()
                    .rounded(px(3.0))
                    .bg(rgb(color))
                    .with_transition("sftp-progress-bar-fill")
                    .transition_when(
                        true,
                        Duration::from_millis(300),
                        EaseInOutCubic,
                        move |el| el.w(px(filled_w)),
                    ),
            ),
    )
}

/// Format a `done / total` byte ratio for display, e.g. "2.1M / 8.0M".
fn format_byte_ratio(done: u64, total: u64) -> String {
    if total == 0 {
        let (d, du) = human_bytes_simple(done);
        format!("{:.1}{}", d, du)
    } else {
        let (d, du) = human_bytes_simple(done);
        let (t, tu) = human_bytes_simple(total);
        format!("{:.1}{} / {:.1}{}", d, du, t, tu)
    }
}

fn human_bytes_simple(bytes: u64) -> (f64, &'static str) {
    let b = bytes as f64;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    if b >= GB {
        (b / GB, "G")
    } else if b >= MB {
        (b / MB, "M")
    } else if b >= KB {
        (b / KB, "K")
    } else {
        (b, "B")
    }
}

/// Truncate a filesystem path in the *middle*, keeping the head (top-level
/// directory) and tail (filename) visible with `…` between them. This is
/// more useful than a head-only or tail-only truncation because the user
/// can tell both *which* top-level project a file belongs to and *what* the
/// file is.
///
/// Examples (max=30):
///   "/home/user/file.txt"              -> "/home/user/file.txt"
///   "/home/user/projects/x/deep/f.txt"  -> "/home/.../deep/f.txt"
///   "very_long_filename_no_slashes.txt" -> "very_long_filen...ses.txt"
fn truncate_path_middle(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    // Split into components on `/`. We keep the first segment (head) and the
    // last segment (tail), and collapse everything in between into a single
    // `…`. If even that doesn't fit, we hard-truncate the tail.
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let is_absolute = path.starts_with('/');
    let prefix = if is_absolute { "/" } else { "" };

    if parts.len() >= 3 {
        let head = parts[0];
        let tail = parts.last().unwrap();
        // "/head/…/tail"
        let candidate = format!("{prefix}{head}/…/{tail}");
        if candidate.len() <= max {
            return candidate;
        }
        // Tail alone is too long — hard-truncate it from the middle too.
        let budget = max.saturating_sub(prefix.len() + head.len() + 4); // "/…/…"
        if budget > 4 {
            let half = budget / 2;
            let t_len = tail.len();
            if t_len > budget {
                let keep_head = &tail[..half];
                let keep_tail = &tail[t_len - half..];
                return format!("{prefix}{head}/…/{keep_head}…{keep_tail}");
            }
        }
        // Fallback: just show head + … + truncated tail.
        let tail_budget = max.saturating_sub(prefix.len() + head.len() + 3); // "/…"
        let cut = tail_budget.saturating_sub(1).max(1);
        return format!("{prefix}{head}/…/{}…", &tail[..cut.min(tail.len())]);
    }

    // No slashes (or very few): hard-truncate the middle of the single
    // segment.
    let cut = max.saturating_sub(1);
    let half = cut / 2;
    let s = path.as_bytes();
    // Be careful not to split a multi-byte char — fall back to char indices.
    let chars: Vec<char> = path.chars().collect();
    if chars.len() > cut {
        let head: String = chars[..half].iter().collect();
        let tail: String = chars[chars.len() - half..].iter().collect();
        return format!("{head}…{tail}");
    }
    let _ = s;
    path.chars().take(cut).collect::<String>() + "…"
}

/// Truncate a filesystem path for display, keeping the basename and
/// prefixing with `…` when the full path exceeds `max` chars. Absolute
/// paths and relative paths are both handled — we split on `/`.
///
/// Deprecated in favour of [`truncate_path_middle`], which preserves both
/// the head and tail of a long path. Kept for any future callers that want
/// the simpler tail-only form.
#[allow(dead_code)]
fn truncate_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    // Find the last `/` and keep everything after it.
    let base = path.rsplit('/').next().unwrap_or(path);
    if base.len() >= max {
        // Even the basename is too long — hard-truncate it.
        let cut = max.saturating_sub(1);
        format!("{}…", &base[..cut])
    } else {
        format!("…/{}", base)
    }
}

// Accent color for the memory progress bar fill — read live so theme
// changes are picked up without a recompile.
fn color_accent() -> u32 {
    term_blue()
}
