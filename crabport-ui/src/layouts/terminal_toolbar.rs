use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutCubic};
use std::time::Duration;

use crabport_terminal::terminal::{MemoryStats, NetworkStats, RemoteMetrics, RemoteStatus};

use crate::color::*;

const TOOLBAR_HEIGHT: f32 = 36.0;

// ---------------------------------------------------------------------------
// Status colors
// ---------------------------------------------------------------------------

fn status_color(status: RemoteStatus) -> u32 {
    match status {
        RemoteStatus::Local => COLOR_LOCAL,
        RemoteStatus::Connected => COLOR_SUCCESS,
        RemoteStatus::Connecting => COLOR_WARNING,
        RemoteStatus::Disconnected => COLOR_ERROR,
    }
}

const COLOR_LOCAL: u32 = 0x585b70;
const COLOR_SUCCESS: u32 = 0xa6e3a1;
const COLOR_WARNING: u32 = 0xf9e2af;
const COLOR_ERROR: u32 = 0xf38ba8;

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
) -> impl IntoElement {
    div()
        .id("terminal-toolbar")
        .w_full()
        .overflow_hidden()
        .border_t_1()
        .with_transition("terminal-toolbar-height")
        .transition_when_else(
            is_terminal,
            Duration::from_millis(500),
            EaseInOutCubic,
            |el| el.h(px(TOOLBAR_HEIGHT)),
            |el| el.h_0(),
        )
        .bg(rgb(BG_TAB_BAR))
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .w_full()
                .h(px(TOOLBAR_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .gap_4()
                .text_color(rgb(TEXT_MUTED))
                .child(render_connection(status, metrics.latency_ms))
                .children(render_memory(metrics.memory))
                .children(render_network(metrics.network)),
        )
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
                    .text_color(rgb(TEXT_MUTED)),
            )
            // Progress bar
            .child(
                div()
                    .w(px(BAR_WIDTH))
                    .h(px(BAR_HEIGHT))
                    .rounded(px(3.0))
                    .bg(rgb(BORDER))
                    .child(
                        div()
                            .h_full()
                            .w(px(filled_w))
                            .rounded(px(3.0))
                            .bg(rgb(COLOR_ACCENT)),
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
                    .text_color(rgb(TEXT_MUTED)),
            )
            .child(div().text_xs().child(format_rate(net.bytes_sent)))
            // Download
            .child(
                svg()
                    .path("icons/terminal-toolbar/arrow-down-to-line.svg")
                    .size(px(12.0))
                    .text_color(rgb(TEXT_MUTED)),
            )
            .child(div().text_xs().child(format_rate(net.bytes_recv))),
    )
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format used/total as e.g. "2.1G / 16G" or "512M / 8G"
fn format_memory(used: u64, total: u64) -> String {
    let (used_val, used_unit) = human_bytes(used);
    let (total_val, total_unit) = human_bytes(total);
    if used_unit == total_unit {
        format!(
            "{:.0}{} / {:.0}{}",
            used_val, used_unit, total_val, total_unit
        )
    } else {
        format!(
            "{:.1}{} / {:.0}{}",
            used_val, used_unit, total_val, total_unit
        )
    }
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

// Accent color for the memory progress bar fill
const COLOR_ACCENT: u32 = 0x89b4fa;
