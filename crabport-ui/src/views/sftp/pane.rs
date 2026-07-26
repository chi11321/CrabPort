//! Shared rendering primitives for the SFTP file panels.
//!
//! Extracted from the near-identical local/remote halves of
//! [`super::panel`] and reused by the sidebar SFTP panel in
//! `views::panel::sftp`. Every function takes the per-side `id_prefix`
//! (or a fully-formed id) so the extracted markup produces exactly the
//! same element / transition IDs as the original inline copies.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::InputState;
use gpui_component::scroll::Scrollbar;

use crate::color::*;
use crate::components::host_selector::PanelSide;
use crate::components::input::StyledInput;
use crate::motion::{EASE_STANDARD, duration_fast};

use super::view::SftpTabView;

// ---------------------------------------------------------------------------
// Side helpers
// ---------------------------------------------------------------------------

/// Per-side element-ID prefix for the SFTP tab panels.
pub(super) fn tab_id_prefix(side: PanelSide) -> &'static str {
    match side {
        PanelSide::Left => "sftp-tab-left",
        PanelSide::Right => "sftp-tab-right",
    }
}

/// The opposite panel side — used for cross-panel operations.
pub(super) fn other_side(side: PanelSide) -> PanelSide {
    match side {
        PanelSide::Left => PanelSide::Right,
        PanelSide::Right => PanelSide::Left,
    }
}

/// Click handler that opens the host-selector overlay for `side`. Shared by
/// the host chip and the disconnected panel's "Select Host" button.
pub(super) fn open_host_selector_handler(
    entity: &WeakEntity<SftpTabView>,
    side: PanelSide,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let entity = entity.clone();
    move |_e, w, cx| {
        let _ = entity.update(cx, |view, cx| {
            view.host_selector_open_for = Some(side);
            let hosts = view.hosts.clone();
            if let Some(ref overlay) = view.host_selector {
                overlay.update(cx, |o, cx| {
                    o.set_hosts(hosts);
                    o.open(w, cx);
                });
            }
            cx.notify();
        });
    }
}

// ---------------------------------------------------------------------------
// Remote path helpers
// ---------------------------------------------------------------------------

/// Join a remote path component onto a remote cwd string, handling the
/// trailing-slash cases. POSIX-style (forward slash).
pub(crate) fn join_remote_path(cwd: &str, name: &str) -> String {
    if cwd.ends_with('/') {
        format!("{}{}", cwd, name)
    } else {
        format!("{}/{}", cwd, name)
    }
}

/// Compute the parent path of a remote cwd string. Returns "/" for root.
pub(crate) fn remote_parent(cwd: &str) -> String {
    let mut parts: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    parts.pop();
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Resolve a listing row's navigation target: `.` stays on the cwd,
/// `..` goes to the parent, anything else joins onto the cwd.
pub(crate) fn remote_target_path(cwd: &str, name: &str) -> String {
    if name == "." {
        cwd.to_string()
    } else if name == ".." {
        remote_parent(cwd)
    } else {
        join_remote_path(cwd, name)
    }
}

// ---------------------------------------------------------------------------
// Listing rows (sort + ".." row + fixed row sizes)
// ---------------------------------------------------------------------------

/// Sort comparator for local listings: directories first, then
/// case-insensitive by name.
pub(crate) fn cmp_dirs_first(
    a: &crabport_sftp::FileEntry,
    b: &crabport_sftp::FileEntry,
) -> std::cmp::Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
}

/// Sort comparator for remote listings: `.` and `..` first, then
/// case-insensitive by name.
pub(crate) fn cmp_remote_listing(
    a: &crabport_sftp::FileEntry,
    b: &crabport_sftp::FileEntry,
) -> std::cmp::Ordering {
    match (a.name.as_str(), b.name.as_str()) {
        (".", _) => std::cmp::Ordering::Less,
        (_, ".") => std::cmp::Ordering::Greater,
        ("..", _) => std::cmp::Ordering::Less,
        (_, "..") => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
}

/// Sort `entries` with `cmp`, prepend the `..` parent-navigation row,
/// and precompute the fixed 26px row-size table for the virtual list.
pub(crate) fn build_entry_rows(
    mut entries: Vec<crabport_sftp::FileEntry>,
    cmp: fn(&crabport_sftp::FileEntry, &crabport_sftp::FileEntry) -> std::cmp::Ordering,
) -> (Rc<Vec<crabport_sftp::FileEntry>>, Rc<Vec<Size<Pixels>>>) {
    entries.sort_by(cmp);
    let mut all_entries: Vec<crabport_sftp::FileEntry> = vec![crabport_sftp::FileEntry {
        name: "..".into(),
        is_dir: true,
        size: None,
        permissions: None,
        modified: None,
    }];
    all_entries.extend(entries);

    let item_sizes = Rc::new(
        all_entries
            .iter()
            .map(|_| Size {
                width: px(0.0),
                height: px(26.0),
            })
            .collect::<Vec<_>>(),
    );
    (Rc::new(all_entries), item_sizes)
}

// ---------------------------------------------------------------------------
// Column formatting helpers
// ---------------------------------------------------------------------------

/// Format a byte count as a human-readable string (e.g. "1.2 KB", "3.4 MB").
/// Returns an empty string for `None` (directories or unavailable).
fn format_size(opt: Option<u64>) -> String {
    match opt {
        None => String::new(),
        Some(bytes) => {
            const KB: f64 = 1024.0;
            const MB: f64 = 1024.0 * 1024.0;
            const GB: f64 = 1024.0 * 1024.0 * 1024.0;
            let b = bytes as f64;
            if b >= GB {
                format!("{:.1} GB", b / GB)
            } else if b >= MB {
                format!("{:.1} MB", b / MB)
            } else if b >= KB {
                format!("{:.1} KB", b / KB)
            } else {
                format!("{} B", bytes)
            }
        }
    }
}

/// Format a Unix timestamp (seconds) as "YYYY-MM-DD HH:MM".
/// Returns an empty string for `None`.
fn format_modified(opt: Option<i64>) -> String {
    match opt {
        None => String::new(),
        Some(secs) => {
            // Civil-time conversion from Unix epoch seconds, no external crate.
            // Algorithm: Howard Hinnant's "days_from_civil" in reverse.
            let days = secs.div_euclid(86400);
            let secs_of_day = secs.rem_euclid(86400) as u32;
            let hour = secs_of_day / 3600;
            let minute = (secs_of_day % 3600) / 60;

            // Convert days since epoch to (y, m, d).
            let z = days + 719468; // epoch 1970-01-01 → civil 0000-03-01
            let era = if z >= 0 { z } else { z - 146096 } / 146097;
            let doe = z - era * 146097; // [0, 146097)
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
            let mp = (5 * doy + 2) / 153; // [0, 11]
            let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
            let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
            let year = if m <= 2 { y + 1 } else { y };

            format!("{:04}-{:02}-{:02} {:02}:{:02}", year, m, d, hour, minute)
        }
    }
}

/// Render the 3 metadata columns (size, permissions, modified) for a file row.
fn render_metadata_columns(
    size: Option<u64>,
    permissions: &Option<String>,
    modified: Option<i64>,
) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .w(px(80.0))
                .text_right()
                .text_xs()
                .text_color(rgb(text_muted()))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(format_size(size)),
        )
        .child(
            div()
                .w(px(90.0))
                .text_xs()
                .text_color(rgb(text_muted()))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(permissions.clone().unwrap_or_default()),
        )
        .child(
            div()
                .w(px(120.0))
                .text_xs()
                .text_color(rgb(text_muted()))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(format_modified(modified)),
        )
}

// ---------------------------------------------------------------------------
// Host selector opening (chip / disconnected button)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Drag-over tracking
// ---------------------------------------------------------------------------

/// `on_drag_move` listener that keeps a panel's `drag_over` flag in sync
/// with whether the cursor is inside the list bounds. Generic over the
/// drag payload type `T`.
pub(super) fn panel_drag_over_tracker<T: 'static>(
    entity: &WeakEntity<SftpTabView>,
    side: PanelSide,
) -> impl Fn(&DragMoveEvent<T>, &mut Window, &mut App) + 'static {
    let entity = entity.clone();
    move |e, _w, cx| {
        let _ = entity.update(cx, |view, cx| {
            let panel = view.panel_mut(side);
            let should = e.bounds.contains(&e.event.position);
            if panel.drag_over != should {
                panel.drag_over = should;
                cx.notify();
            }
        });
    }
}

/// Zero-size canvas that registers a window-level `FileDropEvent`
/// listener during paint. When the OS reports the drag has left the
/// window entirely (`Exited`), `on_exit` runs so the drop-zone overlay
/// doesn't get stuck visible — `on_drag_move` can't catch this because
/// `Exited` is not a `MouseMoveEvent`.
pub(crate) fn render_drag_exit_canvas(on_exit: impl Fn(&mut App) + 'static) -> impl IntoElement {
    let on_exit = Rc::new(on_exit);
    canvas(
        |_bounds, _window, _cx| {},
        move |_bounds, _state, window, _cx| {
            window.on_mouse_event({
                let on_exit = on_exit.clone();
                move |event: &FileDropEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    if matches!(event, FileDropEvent::Exited) {
                        on_exit(cx);
                    }
                }
            });
        },
    )
    .w_0()
    .h_0()
}

// ---------------------------------------------------------------------------
// Row visuals
// ---------------------------------------------------------------------------

/// Persistent blue accent bar on the left edge of a selected row. The
/// bar is always rendered but its opacity is eased in/out via a
/// transition so selection changes animate smoothly.
pub(crate) fn render_selection_bar(bar_id: String, is_selected: bool) -> impl IntoElement {
    let bar_id = ElementId::Name(SharedString::from(bar_id));
    div()
        .id(bar_id.clone())
        .absolute()
        .top(px(2.0))
        .bottom(px(2.0))
        .left_0()
        .w(px(2.0))
        .rounded(px(1.0))
        .bg(rgb(btn_primary_bg()))
        .opacity(0.0)
        .with_transition(bar_id)
        .transition_when_else(
            is_selected,
            duration_fast(),
            EASE_STANDARD,
            |el| el.opacity(1.0),
            |el| el.opacity(0.0),
        )
}

/// Attach the shared visual tail to an SFTP tab file row: hover
/// transition, selection accent bar, type icon, and either the inline
/// rename input or the name label, followed by the metadata columns.
/// Interactive handlers (drag / mouse-down) must already be attached to
/// `row` — `.with_transition` does not forward them.
#[allow(clippy::too_many_arguments)]
pub(super) fn file_row_visuals(
    row: Stateful<Div>,
    transition_id: ElementId,
    id_prefix: &str,
    i: usize,
    name: String,
    entity: WeakEntity<SftpTabView>,
    side: PanelSide,
    is_highlighted: bool,
    is_selected: bool,
    is_renaming: bool,
    rename_input: Option<Entity<InputState>>,
    icon_path: &'static str,
    entry_size: Option<u64>,
    entry_permissions: Option<String>,
    entry_modified: Option<i64>,
) -> impl IntoElement {
    row.with_transition(transition_id)
        .on_hover({
            let name = name.clone();
            move |hovered, _w, cx| {
                let _ = entity.update(cx, |view, cx| {
                    let panel = view.panel_mut(side);
                    if *hovered {
                        panel.hovered = Some(name.clone());
                    } else if panel.hovered.as_deref() == Some(name.as_str()) {
                        panel.hovered = None;
                    }
                    cx.notify();
                });
            }
        })
        .transition_when_else(
            is_highlighted,
            duration_fast(),
            EASE_STANDARD,
            |el| el.bg(rgba((surface_hover() << 8) | 0xFF)),
            |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
        )
        .relative()
        .child(render_selection_bar(
            format!("{id_prefix}-bar-{i}"),
            is_selected,
        ))
        .child(
            svg()
                .path(icon_path)
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(rgb(text_muted())),
        )
        .when_some(
            if is_renaming { rename_input } else { None },
            |el, input| {
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(StyledInput::new(format!("{id_prefix}-rename-{i}"), input).xsmall()),
                )
                .child(render_metadata_columns(
                    entry_size,
                    &entry_permissions,
                    entry_modified,
                ))
            },
        )
        .when(!is_renaming, |el| {
            el.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(text_primary()))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(name.clone()),
            )
            .child(render_metadata_columns(
                entry_size,
                &entry_permissions,
                entry_modified,
            ))
        })
}

// ---------------------------------------------------------------------------
// Scroll + action button chrome
// ---------------------------------------------------------------------------

/// Scrollbar overlay: absolutely positioned on the list's right edge,
/// transparent track, thumb only on hover — so rows keep full width.
pub(crate) fn render_scrollbar_overlay(
    scroll_handle: &VirtualListScrollHandle,
) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(16.0))
        .child(
            Scrollbar::vertical(scroll_handle)
                .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
        )
}
