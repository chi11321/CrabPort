//! Panel rendering for `SftpTabView`.
//!
//! A single `render_panel` method handles both left and right panels,
//! each of which can be local or remote. This is a separate `impl` block
//! from the one in [`super::view`]. Rust allows multiple `impl` blocks
//! per type.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::scroll::Scrollbar;
use gpui_component::v_virtual_list;
use rust_i18n::t;

use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertSeverity, AlertState};
use crate::components::drop_zone_overlay::DropZoneOverlay;
use crate::components::host_selector::PanelSide;
use crate::components::input::StyledInput;

use super::drag::LocalFileDragValue;
use super::drag::SftpDragValue;
use super::helpers::render_action_button;
use super::helpers::{
    trigger_batch_download, trigger_remote_download_from_button, trigger_remote_to_remote_transfer,
    trigger_upload, trigger_upload_from_local,
};
use super::view::{PanelHost, SftpTabView, join_remote_path, remote_parent};

// -----------------------------------------------------------------------
// Column formatting helpers
// -----------------------------------------------------------------------

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

/// Render the column header row above the file list. Sticky at the top
/// of the scroll area.
///
/// The layout MUST match the file row layout exactly: icon (14px) + gap
/// (6px) + name (flex_1) + size (80px) + permissions (90px) + modified
/// (120px). Both use `px_2()` and `gap_1p5()` so the columns align.
fn render_column_header(id_prefix: &str) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("{id_prefix}-header")))
        .w_full()
        .h(px(26.0))
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .px_2()
        .bg(rgb(bg_base()))
        .border_b_1()
        .border_color(rgb(border()))
        // Icon spacer — matches the 14px icon in each row.
        .child(div().w(px(14.0)).flex_shrink_0())
        // Name column (flex_1) — matches the row's name container.
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("sftp_tab.col_name").to_string()),
        )
        // Size column (80px, right-aligned).
        .child(
            div()
                .w(px(80.0))
                .text_right()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("sftp_tab.col_size").to_string()),
        )
        // Permissions column (90px).
        .child(
            div()
                .w(px(90.0))
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("sftp_tab.col_permissions").to_string()),
        )
        // Modified column (120px).
        .child(
            div()
                .w(px(120.0))
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(t!("sftp_tab.col_modified").to_string()),
        )
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

impl SftpTabView {
    /// Render a single panel (left or right). The panel can be local or
    /// remote — the render logic adapts based on `PanelHost`.
    pub(super) fn render_panel(
        &mut self,
        side: PanelSide,
        entity: &WeakEntity<Self>,
        tooltip_ctrl: &Option<Entity<crate::components::tooltip::TooltipController>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.panel(side);
        match &panel.host {
            PanelHost::Disconnected => self
                .render_disconnected_panel(side, entity, tooltip_ctrl, _window, cx)
                .into_any_element(),
            PanelHost::Remote { .. } => self
                .render_remote_panel(side, entity, tooltip_ctrl, _window, cx)
                .into_any_element(),
            PanelHost::Local => self
                .render_local_panel(side, entity, tooltip_ctrl, _window, cx)
                .into_any_element(),
        }
    }

    // -----------------------------------------------------------------------
    // Disconnected panel
    // -----------------------------------------------------------------------

    pub(super) fn render_disconnected_panel(
        &mut self,
        side: PanelSide,
        entity: &WeakEntity<Self>,
        _tooltip_ctrl: &Option<Entity<crate::components::tooltip::TooltipController>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id_prefix = match side {
            PanelSide::Left => "sftp-tab-left",
            PanelSide::Right => "sftp-tab-right",
        };

        div()
            .h_full()
            .w_full()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(rgb(bg_base()))
            .gap_3()
            .child(
                svg()
                    .path("icons/server.svg")
                    .size(px(48.0))
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .child(t!("sftp_tab.disconnected_hint").to_string()),
            )
            .child(
                Button::new(ElementId::Name(
                    format!("{id_prefix}-select-host-btn").into(),
                ))
                .primary()
                .w(px(140.0))
                .centered(true)
                .child(t!("sftp_tab.select_host_btn").to_string())
                .on_click({
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
                }),
            )
    }

    // -----------------------------------------------------------------------
    // Local panel
    // -----------------------------------------------------------------------

    pub(super) fn render_local_panel(
        &mut self,
        side: PanelSide,
        entity: &WeakEntity<Self>,
        tooltip_ctrl: &Option<Entity<crate::components::tooltip::TooltipController>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self.panel(side);
        // Sort + prepend ".."
        let mut sorted: Vec<crabport_sftp::FileEntry> =
            panel.local_entries.iter().cloned().collect();
        sorted.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        let mut all_entries: Vec<crabport_sftp::FileEntry> = vec![crabport_sftp::FileEntry {
            name: "..".into(),
            is_dir: true,
            size: None,
            permissions: None,
            modified: None,
        }];
        all_entries.extend(sorted);

        let item_sizes = Rc::new(
            all_entries
                .iter()
                .map(|_| Size {
                    width: px(0.0),
                    height: px(26.0),
                })
                .collect::<Vec<_>>(),
        );
        let all_entries = Rc::new(all_entries);
        let scroll_handle = panel.scroll.clone();
        let local_cwd = panel.local_cwd.clone();
        let path_input = panel.path_input.clone();

        // The "other" panel — for cross-panel operations
        let other_side = match side {
            PanelSide::Left => PanelSide::Right,
            PanelSide::Right => PanelSide::Left,
        };
        let other_panel = self.panel(other_side);
        let _other_is_remote = other_panel.host.is_remote();
        let other_on_download = other_panel.on_download.clone();

        let on_upload = panel.on_upload.clone();
        let _on_download = panel.on_download.clone();
        let _on_upload_for_drop = on_upload.clone();
        let drag_over = panel.drag_over;
        let entity_for_drop = entity.clone();
        let entity_for_list = entity.clone();
        let renaming_entry = panel.renaming.clone();
        let rename_input = panel.rename_input.clone();

        let id_prefix = match side {
            PanelSide::Left => "sftp-tab-left",
            PanelSide::Right => "sftp-tab-right",
        };

        div()
            .h_full()
            .w_full()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .bg(rgb(bg_base()))
            .pt_1()
            .px_1()
            .relative()
            // Host selector + path bar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mb_1()
                    // Host label button (always "Local" for local panel)
                    .child(
                        div()
                            .id(SharedString::from(format!("{id_prefix}-host")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(26.0))
                            .rounded(px(4.0))
                            .bg(rgb(surface_hover()))
                            .on_click({
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
                            })
                            .child(
                                svg()
                                    .path("icons/folder.svg")
                                    .size(px(12.0))
                                    .text_color(rgb(text_muted())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_primary()))
                                    .child(t!("sftp_tab.local").to_string()),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().when_some(path_input.clone(), |el, input| {
                            el.child(
                                StyledInput::new(
                                    SharedString::from(format!("{id_prefix}-path")),
                                    input,
                                )
                                .xsmall()
                                .prefix(
                                    svg()
                                        .path("icons/folder.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(text_muted())),
                                ),
                            )
                        }),
                    ),
            )
            // Action button row: refresh / mkdir
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mb_1()
                    .child(render_action_button(
                        SharedString::from(format!("{id_prefix}-refresh")),
                        "icons/refresh-cw.svg",
                        t!("sftp.refresh").to_string(),
                        true,
                        tooltip_ctrl.clone(),
                        {
                            let entity = entity.clone();
                            move |_w, cx| {
                                let _ = entity.update(cx, |view, cx| {
                                    let panel = view.panel_mut(side);
                                    panel.local_entries =
                                        SftpTabView::read_local_dir(&panel.local_cwd);
                                    panel.selected.clear();
                                    cx.notify();
                                });
                            }
                        },
                    ))
                    .child(render_action_button(
                        SharedString::from(format!("{id_prefix}-mkdir")),
                        "icons/plus.svg",
                        t!("sftp_tab.mkdir").to_string(),
                        true,
                        tooltip_ctrl.clone(),
                        {
                            let entity = entity.clone();
                            move |_w, cx| {
                                let _ = entity.update(cx, |view, cx| {
                                    // Create "New Folder" with a unique name.
                                    let panel = view.panel_mut(side);
                                    let base = "New Folder";
                                    let mut name = base.to_string();
                                    let mut i = 1;
                                    while panel.local_cwd.join(&name).exists() {
                                        name = format!("{base} ({i})");
                                        i += 1;
                                    }
                                    let path = panel.local_cwd.join(&name);
                                    let _ = std::fs::create_dir(&path);
                                    panel.local_entries =
                                        SftpTabView::read_local_dir(&panel.local_cwd);
                                    cx.notify();
                                });
                            }
                        },
                    )),
            )
            // Column header — outside the scroll container so the
            // scrollbar (absolute top_0..bottom_0 inside the container
            // below) only spans the file-list area, not the header.
            .child(render_column_header(id_prefix))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    // Drop zone for remote→local drag (download) or
                    // remote→remote drag (download to temp then upload).
                    .on_drop::<SftpDragValue>(move |drag, _w, cx| {
                        let entity = entity_for_drop.clone();
                        let on_download = other_on_download.clone();
                        let local_cwd = local_cwd.clone();
                        let _ = entity.update(cx, |view, cx| {
                            let panel = view.panel_mut(side);
                            panel.drag_over = false;
                            cx.notify();
                        });
                        // Same-panel drop: no-op.
                        if drag.source_side == side {
                            return;
                        }
                        // The drag came from a remote panel (the other
                        // panel). Use the source panel's on_download to
                        // download the file to this local panel's cwd.
                        if let Some(cb) = on_download {
                            let local_dest = local_cwd.join(&drag.name);
                            cb(
                                drag.remote_path.clone(),
                                local_dest.to_string_lossy().into_owned(),
                                cx,
                            );
                        }
                    })
                    .on_drag_move::<SftpDragValue>({
                        let entity = entity_for_list.clone();
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
                    })
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            SharedString::from(format!("{id_prefix}-entries")),
                            item_sizes.clone(),
                            move |this, range, _window, cx| {
                                let all_entries = &all_entries;
                                range
                                    .map(|i| {
                                        let entry = &all_entries[i];
                                        let name = entry.name.clone();
                                        let is_dir = entry.is_dir;
                                        let icon_path = if is_dir {
                                            "icons/folder.svg"
                                        } else {
                                            "icons/file.svg"
                                        };
                                        let entry_size = entry.size;
                                        let entry_permissions = entry.permissions.clone();
                                        let entry_modified = entry.modified;

                                        let cwd_ref = this.panel(side).local_cwd.clone();
                                        let target_path: PathBuf = if name == ".." {
                                            cwd_ref.parent().unwrap_or(&cwd_ref).to_path_buf()
                                        } else {
                                            cwd_ref.join(&name)
                                        };

                                        let entity = cx.entity().downgrade();
                                        let context_menu = this.context_menu.clone();
                                        let alert_controller = this.alert_controller.clone();
                                        let other_side = match side {
                                            PanelSide::Left => PanelSide::Right,
                                            PanelSide::Right => PanelSide::Left,
                                        };
                                        let other_on_upload_for_ctx =
                                            this.panel(other_side).on_upload.clone();
                                        let other_on_upload_batch_for_ctx =
                                            this.panel(other_side).on_upload_batch.clone();
                                        let other_remote_cwd_for_ctx =
                                            this.panel(other_side).remote_cwd.clone();
                                        let other_is_remote =
                                            this.panel(other_side).host.is_remote();
                                        let is_hovered =
                                            this.panel(side).hovered.as_deref() == Some(name.as_str());
                                        let force_highlight =
                                            this.panel(side).context_menu_entry.as_deref()
                                                == Some(name.as_str());
                                        let is_selected =
                                            this.panel(side).selected.contains(name.as_str())
                                                && name != "..";
                                        let is_highlighted = is_hovered || force_highlight;
                                        let is_renaming =
                                            renaming_entry.as_deref() == Some(name.as_str());
                                        let row_rename_input = rename_input.clone();
                                        let row_id = ElementId::Name(
                                            format!("{id_prefix}-{i}").into(),
                                        );
                                        let row_id_for_transition = row_id.clone();
                                        let draggable = name != "..";
                                        let drag_local_path = target_path.to_string_lossy()
                                            .into_owned();
                                        let drag_name = name.clone();
                                        let drag_is_dir = is_dir;
                                        let drag_source_side = side;

                                        div()
                                            .id(row_id.clone())
                                            .h(px(26.0))
                                            .w_full()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1p5()
                                            .px_2()
                                            .rounded(px(4.0))
                                            .when(draggable, |el| {
                                                el.on_drag(
                                                    LocalFileDragValue {
                                                        local_path: drag_local_path.clone(),
                                                        name: drag_name.clone(),
                                                        is_dir: drag_is_dir,
                                                        source_side: drag_source_side,
                                                    },
                                                    |drag_value, _offset, _w, cx| {
                                                        cx.new(|_| drag_value.clone())
                                                    },
                                                )
                                            })
                                            .on_mouse_down(MouseButton::Left, {
                                                let name = name.clone();
                                                let is_dir = is_dir;
                                                let target = target_path.clone();
                                                let entity = entity.clone();
                                                move |event, _w, cx| {
                                                    if is_dir && event.click_count == 2 {
                                                        let _ = entity.update(cx, |view, cx| {
                                                            view.local_navigate(side, target.clone(), cx);
                                                        });
                                                        return;
                                                    }
                                                    if name == ".." {
                                                        return;
                                                    }
                                                    let _ = entity.update(cx, |view, cx| {
                                                        let panel = view.panel_mut(side);
                                                        if event.modifiers.secondary() {
                                                            if panel.selected.contains(name.as_str()) {
                                                                panel.selected.remove(name.as_str());
                                                            } else {
                                                                panel.selected.insert(name.clone());
                                                            }
                                                        } else {
                                                            panel.selected.clear();
                                                            panel.selected.insert(name.clone());
                                                        }
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .on_mouse_down(MouseButton::Right, {
                                                let name = name.clone();
                                                let target_path = target_path.clone();
                                                let entity = entity.clone();
                                                move |event, _w, cx| {
                                                    let Some(ref cm) = context_menu else {
                                                        return;
                                                    };
                                                    let pos = event.position;
                                                    let menu_entries = entity
                                                        .update(cx, |view, cx| -> Vec<(String, bool, String)> {
                                                            let panel = view.panel_mut(side);
                                                            if !panel.selected.contains(name.as_str()) {
                                                                panel.selected.clear();
                                                                if name != ".." {
                                                                    panel.selected.insert(name.clone());
                                                                }
                                                            }
                                                            panel.context_menu_entry = Some(name.clone());
                                                            cx.notify();
                                                            let cwd_str = panel.local_cwd.to_string_lossy().into_owned();
                                                            panel.local_entries
                                                                .iter()
                                                                .filter(|e| {
                                                                    e.name != ".."
                                                                        && panel.selected.contains(e.name.as_str())
                                                                })
                                                                .map(|e| {
                                                                    let p = cwd_str.clone();
                                                                    let p = if p.ends_with('/') {
                                                                        format!("{}{}", p, e.name)
                                                                    } else {
                                                                        format!("{}/{}", p, e.name)
                                                                    };
                                                                    (e.name.clone(), e.is_dir, p)
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();
                                                    let mut items: Vec<ContextMenuItem> = Vec::new();

                                                    // Upload selected local files to the other panel's remote host.
                                                    if !menu_entries.is_empty() && other_is_remote && other_on_upload_for_ctx.is_some() {
                                                        let to_upload = menu_entries.clone();
                                                        let on_upload = other_on_upload_for_ctx.clone();
                                                        let on_upload_batch = other_on_upload_batch_for_ctx.clone();
                                                        let remote_cwd = other_remote_cwd_for_ctx.clone();
                                                        let entity_for_clear = entity.clone();
                                                        let count = to_upload.len();
                                                        let label = if count == 1 {
                                                            t!("sftp.upload").to_string()
                                                        } else {
                                                            t!("sftp.upload_n", count = count).to_string()
                                                        };
                                                        items.push(ContextMenuItem::new(label, move |_w, cx| {
                                                            let _ = entity_for_clear.update(cx, |view, cx| {
                                                                view.panel_mut(side).selected.clear();
                                                                cx.notify();
                                                            });
                                                            let on_upload = on_upload.clone();
                                                            let on_upload_batch = on_upload_batch.clone();
                                                            let remote_cwd = remote_cwd.clone();
                                                            if let (Some(cb), Some(rwd)) = (on_upload, remote_cwd) {
                                                                let rwd = rwd.as_str().to_string();
                                                                let items: Vec<(String, String)> = to_upload
                                                                    .iter()
                                                                    .map(|(n, _, local_path)| {
                                                                        let remote = join_remote_path(&rwd, n);
                                                                        (local_path.clone(), remote)
                                                                    })
                                                                    .collect();
                                                                if items.len() == 1 {
                                                                    cb(items[0].0.clone(), items[0].1.clone(), cx);
                                                                } else if let Some(batch_cb) = on_upload_batch {
                                                                    batch_cb(items, cx);
                                                                } else {
                                                                    for (local, remote) in &items {
                                                                        cb(local.clone(), remote.clone(), cx);
                                                                    }
                                                                }
                                                            }
                                                        }));
                                                    }

                                                    // Rename (single selection only).
                                                    if menu_entries.len() == 1 && name != ".." {
                                                        let entry_name = menu_entries[0].0.clone();
                                                        let entity_for_rename = entity.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.rename").to_string(),
                                                            move |window, cx| {
                                                                let _ = entity_for_rename.update(cx, |view, cx| {
                                                                    view.start_local_rename(side, entry_name.clone(), window, cx);
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    // Delete.
                                                    if !menu_entries.is_empty() && name != ".." {
                                                        let to_delete = menu_entries.clone();
                                                        let alert_controller = alert_controller.clone();
                                                        let entity_for_delete = entity.clone();
                                                        items.push(
                                                            ContextMenuItem::new(t!("sftp.delete").to_string(), move |_w, cx| {
                                                                let Some(ref ac) = alert_controller else { return };
                                                                let to_delete = to_delete.clone();
                                                                let entity_for_clear = entity_for_delete.clone();
                                                                ac.update(cx, |c, cx| {
                                                                    c.show(
                                                                        AlertState {
                                                                            severity: AlertSeverity::Danger,
                                                                            title: t!("sftp.delete_title").to_string().into(),
                                                                            description: Some(
                                                                                t!("sftp.delete_prompt", name = to_delete[0].0.as_str())
                                                                                    .to_string()
                                                                                    .into(),
                                                                            ),
                                                                            confirm_label: t!("sftp.delete_confirm").to_string().into(),
                                                                            cancel_label: t!("terminal.host_key_cancel").to_string().into(),
                                                                            on_confirm: Some(Rc::new(move |_w, cx| {
                                                                                for (_, _, p) in &to_delete {
                                                                                    let path = std::path::PathBuf::from(p);
                                                                                    if path.is_dir() {
                                                                                        let _ = std::fs::remove_dir_all(&path);
                                                                                    } else {
                                                                                        let _ = std::fs::remove_file(&path);
                                                                                    }
                                                                                }
                                                                                let _ = entity_for_clear.update(cx, |view, cx| {
                                                                                    let panel = view.panel_mut(side);
                                                                                    panel.local_entries =
                                                                                        SftpTabView::read_local_dir(&panel.local_cwd);
                                                                                    panel.selected.clear();
                                                                                    cx.notify();
                                                                                });
                                                                            })),
                                                                            ..AlertState::default()
                                                                        },
                                                                        cx,
                                                                    );
                                                                });
                                                            }).danger(true),
                                                        );
                                                    }

                                                    if items.is_empty() && name == ".." {
                                                        let target = target_path.clone();
                                                        let entity_for_enter = entity.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.enter").to_string(),
                                                            move |_w, cx| {
                                                                let _ = entity_for_enter.update(cx, |view, cx| {
                                                                    view.local_navigate(side, target.clone(), cx);
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    cm.update(cx, |c, cx| {
                                                        c.show(
                                                            ContextMenuState {
                                                                position: pos,
                                                                items,
                                                                ..ContextMenuState::default()
                                                            },
                                                            cx,
                                                        );
                                                    });
                                                }
                                            })
                                            .with_transition(row_id_for_transition)
                                            .on_hover({
                                                let name = name.clone();
                                                move |hovered, _w, cx| {
                                                    let _ = entity.update(cx, |view, cx| {
                                                        let panel = view.panel_mut(side);
                                                        if *hovered {
                                                            panel.hovered = Some(name.clone());
                                                        } else if panel.hovered.as_deref()
                                                            == Some(name.as_str())
                                                        {
                                                            panel.hovered = None;
                                                        }
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .transition_when_else(
                                                is_highlighted,
                                                Duration::from_millis(120),
                                                Linear,
                                                |el| el.bg(rgba((surface_hover() << 8) | 0xFF)),
                                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                                            )
                                            .relative()
                                            .child(
                                                div()
                                                    .id(ElementId::Name(format!("{id_prefix}-bar-{i}").into()))
                                                    .absolute()
                                                    .top(px(2.0))
                                                    .bottom(px(2.0))
                                                    .left_0()
                                                    .w(px(2.0))
                                                    .rounded(px(1.0))
                                                    .bg(rgb(btn_primary_bg()))
                                                    .opacity(0.0)
                                                    .with_transition(ElementId::Name(format!("{id_prefix}-bar-{i}").into()))
                                                    .transition_when_else(
                                                        is_selected,
                                                        Duration::from_millis(120),
                                                        Linear,
                                                        |el| el.opacity(1.0),
                                                        |el| el.opacity(0.0),
                                                    ),
                                            )
                                            .child(
                                                svg()
                                                    .path(icon_path)
                                                    .size(px(14.0))
                                                    .flex_shrink_0()
                                                    .text_color(rgb(text_muted())),
                                            )
                                            .when_some(
                                                if is_renaming { row_rename_input } else { None },
                                                |el, input| {
                                                    el.child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                StyledInput::new(
                                                                    format!("{id_prefix}-rename-{i}"),
                                                                    input,
                                                                ).xsmall(),
                                                            ),
                                                    )
                                                    .child(render_metadata_columns(
                                                        entry_size,
                                                        &entry_permissions,
                                                        entry_modified,
                                                    ))
                                                },
                                            )
                                            .when(!is_renaming, |el| {
                                                el
                                                    .child(
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
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&scroll_handle),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&scroll_handle)
                                    .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
                            ),
                    )
                    .child(
                        DropZoneOverlay::new(drag_over)
                            .hint(t!("sftp_tab.drop_download_hint").to_string())
                            .id(SharedString::from(format!("{id_prefix}-drop-overlay"))),
                    )
                    // Canvas to catch FileDropEvent::Exited for external drags.
                    .child({
                        let entity = entity_for_list.clone();
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |_bounds, _state, window, _cx| {
                                window.on_mouse_event({
                                    let entity = entity.clone();
                                    move |event: &FileDropEvent, phase, _window, cx| {
                                        if phase != DispatchPhase::Capture {
                                            return;
                                        }
                                        if matches!(event, FileDropEvent::Exited) {
                                            let _ = entity.update(cx, |view, cx| {
                                                let panel = view.panel_mut(side);
                                                if panel.drag_over {
                                                    panel.drag_over = false;
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    }
                                });
                            },
                        )
                        .w_0()
                        .h_0()
                    }),
            )
    }

    // -----------------------------------------------------------------------
    // Remote panel
    // -----------------------------------------------------------------------

    pub(super) fn render_remote_panel(
        &mut self,
        side: PanelSide,
        entity: &WeakEntity<Self>,
        tooltip_ctrl: &Option<Entity<crate::components::tooltip::TooltipController>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self.panel(side);
        // Sort entries: ".", "..", dirs, files — same as sftp.rs.
        let mut sorted: Vec<crabport_sftp::FileEntry> =
            panel.remote_entries.iter().cloned().collect();
        sorted.sort_by(|a, b| match (a.name.as_str(), b.name.as_str()) {
            (".", _) => std::cmp::Ordering::Less,
            (_, ".") => std::cmp::Ordering::Greater,
            ("..", _) => std::cmp::Ordering::Less,
            (_, "..") => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        let mut all_entries: Vec<crabport_sftp::FileEntry> = vec![crabport_sftp::FileEntry {
            name: "..".into(),
            is_dir: true,
            size: None,
            permissions: None,
            modified: None,
        }];
        all_entries.extend(sorted);

        let item_sizes = Rc::new(
            all_entries
                .iter()
                .map(|_| Size {
                    width: px(0.0),
                    height: px(26.0),
                })
                .collect::<Vec<_>>(),
        );
        let all_entries = Rc::new(all_entries);
        let scroll_handle = panel.scroll.clone();
        let cwd = panel.remote_cwd.clone();
        let path_input = panel.path_input.clone();
        let on_navigate = panel.on_navigate.clone();
        let on_download = panel.on_download.clone();
        let on_upload = panel.on_upload.clone();
        let on_upload_batch = panel.on_upload_batch.clone();
        let _on_delete = panel.on_delete.clone();
        let _on_rename = panel.on_rename.clone();
        let _on_edit = panel.on_edit.clone();
        let on_upload_for_drop = on_upload.clone();
        let on_upload_batch_for_drop = on_upload_batch.clone();
        let on_upload_for_r2r = on_upload.clone();
        let on_upload_batch_for_r2r = on_upload_batch.clone();
        let cwd_for_drop_1 = cwd.clone();
        let cwd_for_drop_2 = cwd.clone();
        let cwd_for_r2r_drop = cwd.clone();
        let drag_over = panel.drag_over;
        let connect_count = panel.connect_count;
        let entity_for_drop_1 = entity.clone();
        let entity_for_drop_2 = entity.clone();
        let entity_for_r2r_drop = entity.clone();
        let entity_for_list = entity.clone();
        let renaming_entry = panel.renaming.clone();
        let rename_input = panel.rename_input.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();

        // The "other" panel — for remote-to-remote drag transfers.
        let other_side = match side {
            PanelSide::Left => PanelSide::Right,
            PanelSide::Right => PanelSide::Left,
        };
        let other_panel = self.panel(other_side);
        let other_is_remote = other_panel.host.is_remote();
        let other_on_download_for_r2r = other_panel.on_download.clone();

        let host_label = match &panel.host {
            PanelHost::Remote { host_name, .. } => host_name.clone(),
            _ => t!("sftp_tab.no_host").to_string(),
        };

        // The terminal for rendering (zero-size, keeps frame pump alive)
        let terminal_entity = panel.host.terminal().cloned();

        let id_prefix = match side {
            PanelSide::Left => "sftp-tab-left",
            PanelSide::Right => "sftp-tab-right",
        };

        div()
            .h_full()
            .w_full()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .bg(rgb(bg_base()))
            .pt_1()
            .px_1()
            .relative()
            // Host selector + path bar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mb_1()
                    .child(
                        div()
                            .id(SharedString::from(format!("{id_prefix}-host")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(26.0))
                            .rounded(px(4.0))
                            .bg(rgb(surface_hover()))
                            .on_click({
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
                            })
                            .child(
                                svg()
                                    .path("icons/server.svg")
                                    .size(px(12.0))
                                    .text_color(rgb(text_muted())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_primary()))
                                    .child(host_label),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().when_some(path_input.clone(), |el, input| {
                            el.child(
                                StyledInput::new(
                                    SharedString::from(format!("{id_prefix}-path")),
                                    input,
                                )
                                .xsmall()
                                .prefix(
                                    svg()
                                        .path("icons/folder.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(text_muted())),
                                ),
                            )
                        }),
                    ),
            )
            // Action buttons: download / upload / refresh
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mb_1()
                    .child(render_action_button(
                        SharedString::from(format!("{id_prefix}-download")),
                        "icons/download.svg",
                        t!("sftp.download").to_string(),
                        on_download.is_some(),
                        tooltip_ctrl.clone(),
                        {
                            let entity = entity.clone();
                            let on_download = on_download.clone();
                            let cwd = cwd.clone();
                            move |_w, cx| {
                                trigger_remote_download_from_button(
                                    entity.clone(),
                                    side,
                                    on_download.as_ref(),
                                    cwd.as_ref(),
                                    cx,
                                );
                            }
                        },
                    ))
                    .child(render_action_button(
                        SharedString::from(format!("{id_prefix}-upload")),
                        "icons/upload.svg",
                        t!("sftp.upload").to_string(),
                        on_upload.is_some(),
                        tooltip_ctrl.clone(),
                        {
                            let entity = entity.clone();
                            let on_upload = on_upload.clone();
                            let on_upload_batch = on_upload_batch.clone();
                            let cwd = cwd.clone();
                            move |_w, cx| {
                                trigger_upload(
                                    entity.clone(),
                                    side,
                                    on_upload.as_ref(),
                                    on_upload_batch.as_ref(),
                                    cwd.as_ref(),
                                    cx,
                                );
                            }
                        },
                    ))
                    .child(render_action_button(
                        SharedString::from(format!("{id_prefix}-refresh")),
                        "icons/refresh-cw.svg",
                        t!("sftp.refresh").to_string(),
                        on_navigate.is_some(),
                        tooltip_ctrl.clone(),
                        {
                            let on_navigate = on_navigate.clone();
                            let cwd = cwd.clone();
                            move |_w, cx| {
                                let cb = on_navigate.as_ref();
                                let cwd = cwd.as_ref();
                                if let (Some(cb), Some(cwd)) = (cb, cwd) {
                                    cb(cwd.as_str().to_string(), cx);
                                }
                            }
                        },
                    )),
            )
            // Column header — outside the scroll container so the
            // scrollbar (absolute top_0..bottom_0 inside the container
            // below) only spans the file-list area, not the header.
            .child(render_column_header(id_prefix))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    // Drop zone for local→remote drag (upload) + external files.
                    .on_drop::<LocalFileDragValue>(move |drag, _w, cx| {
                        let on_upload = on_upload.clone();
                        let cwd = cwd_for_drop_1.clone();
                        let entity = entity_for_drop_1.clone();
                        let _ = entity.update(cx, |view, cx| {
                            let panel = view.panel_mut(side);
                            panel.drag_over = false;
                            cx.notify();
                        });
                        // Same-panel drop: no-op.
                        if drag.source_side == side {
                            return;
                        }
                        let on_upload = on_upload.clone();
                        let cwd = cwd.clone();
                        if let (Some(cb), Some(rwd)) = (on_upload, cwd) {
                            let rwd = rwd.as_str().to_string();
                            let remote = join_remote_path(&rwd, &drag.name);
                            cb(drag.local_path.clone(), remote, cx);
                        }
                    })
                    .on_drop::<ExternalPaths>(move |paths, _w, cx| {
                        let on_upload = on_upload_for_drop.clone();
                        let on_upload_batch = on_upload_batch_for_drop.clone();
                        let cwd = cwd_for_drop_2.clone();
                        let entity = entity_for_drop_2.clone();
                        if let Some(cwd) = cwd {
                            let cwd_str = cwd.as_str().to_string();
                            let _ = entity.update(cx, |view, cx| {
                                let panel = view.panel_mut(side);
                                panel.drag_over = false;
                                cx.notify();
                            });
                            let items: Vec<(String, String)> = paths
                                .paths()
                                .into_iter()
                                .map(|local| {
                                    let name = local
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| local.to_string_lossy().into_owned());
                                    let remote = join_remote_path(&cwd_str, &name);
                                    (local.to_string_lossy().into_owned(), remote)
                                })
                                .collect();
                            if items.is_empty() {
                                return;
                            }
                            if items.len() == 1 {
                                if let Some(cb) = on_upload {
                                    cb(items[0].0.clone(), items[0].1.clone(), cx);
                                }
                            } else if let Some(batch_cb) = on_upload_batch {
                                batch_cb(items, cx);
                            } else {
                                for (local, remote) in &items {
                                    if let Some(cb) = on_upload.as_ref() {
                                        cb(local.clone(), remote.clone(), cx);
                                    }
                                }
                            }
                        }
                    })
                    .on_drag_move::<LocalFileDragValue>({
                        let entity = entity_for_list.clone();
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
                    })
                    .on_drag_move::<ExternalPaths>({
                        let entity = entity_for_list.clone();
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
                    })
                    // Drop zone for remote→remote drag: download from
                    // source remote to temp, then upload to this remote.
                    .on_drop::<SftpDragValue>(move |drag, _w, cx| {
                        let entity = entity_for_r2r_drop.clone();
                        let on_upload = on_upload_for_r2r.clone();
                        let on_upload_batch = on_upload_batch_for_r2r.clone();
                        let cwd = cwd_for_r2r_drop.clone();
                        let other_on_download = other_on_download_for_r2r.clone();
                        let _ = entity.update(cx, |view, cx| {
                            let panel = view.panel_mut(side);
                            panel.drag_over = false;
                            cx.notify();
                        });
                        // Same-panel drop: no-op.
                        if drag.source_side == side {
                            return;
                        }
                        // Only proceed if the source panel is remote (it
                        // should be, since only remote panels emit
                        // SftpDragValue).
                        if !other_is_remote {
                            return;
                        }
                        let entries = vec![(
                            drag.name.clone(),
                            drag.is_dir,
                            drag.remote_path.clone(),
                        )];
                        trigger_remote_to_remote_transfer(
                            entries,
                            other_on_download.as_ref(),
                            on_upload.as_ref(),
                            on_upload_batch.as_ref(),
                            cwd.as_ref(),
                            cx,
                        );
                    })
                    .on_drag_move::<SftpDragValue>({
                        let entity = entity_for_list.clone();
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
                    })
                    // Hidden terminal view (zero size, keeps frame pump alive)
                    .when_some(terminal_entity.clone(), |el, term| {
                        el.child(div().w_0().h_0().overflow_hidden().child(term))
                    })
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            SharedString::from(format!("{id_prefix}-entries")),
                            item_sizes.clone(),
                            move |this, range, _window, cx| {
                                let all_entries = &all_entries;
                                range
                                    .map(|i| {
                                        let entry = &all_entries[i];
                                        let name = entry.name.clone();
                                        let is_dir = entry.is_dir;
                                        let icon_path = if is_dir {
                                            "icons/folder.svg"
                                        } else {
                                            "icons/file.svg"
                                        };
                                        let entry_size = entry.size;
                                        let entry_permissions = entry.permissions.clone();
                                        let entry_modified = entry.modified;

                                        let cwd_ref = this.panel(side).remote_cwd.as_ref().map(|s| s.as_str()).unwrap_or("/");
                                        let target_path = if name == "." {
                                            cwd_ref.to_string()
                                        } else if name == ".." {
                                            remote_parent(cwd_ref)
                                        } else {
                                            join_remote_path(cwd_ref, &name)
                                        };

                                        let on_navigate = this.panel(side).on_navigate.clone();
                                        let on_download = this.panel(side).on_download.clone();
                                        let on_delete = this.panel(side).on_delete.clone();
                                        let on_rename = this.panel(side).on_rename.clone();
                                        let on_edit = this.panel(side).on_edit.clone();
                                        let on_upload_for_ctx = this.panel(side).on_upload.clone();
                                        let on_upload_batch_for_ctx = this.panel(side).on_upload_batch.clone();
                                        let remote_cwd_for_ctx = this.panel(side).remote_cwd.clone();

                                        // The "other" panel's state for cross-panel ops
                                        let other_side = match side {
                                            PanelSide::Left => PanelSide::Right,
                                            PanelSide::Right => PanelSide::Left,
                                        };
                                        let other_is_remote = this.panel(other_side).host.is_remote();
                                        let other_local_entries = this.panel(other_side).local_entries.clone();
                                        let other_local_cwd = this.panel(other_side).local_cwd.clone();
                                        let other_on_upload = this.panel(other_side).on_upload.clone();
                                        let other_on_upload_batch = this.panel(other_side).on_upload_batch.clone();
                                        let other_remote_cwd = this.panel(other_side).remote_cwd.clone();

                                        let context_menu = context_menu.clone();
                                        let alert_controller = alert_controller.clone();
                                        let entity = cx.entity().downgrade();
                                        let is_hovered =
                                            this.panel(side).hovered.as_deref() == Some(name.as_str());
                                        let force_highlight =
                                            this.panel(side).context_menu_entry.as_deref()
                                                == Some(name.as_str());
                                        let is_selected =
                                            this.panel(side).selected.contains(name.as_str()) && name != "..";
                                        let is_highlighted = is_hovered || force_highlight;
                                        let is_renaming =
                                            renaming_entry.as_deref() == Some(name.as_str());
                                        let row_rename_input = rename_input.clone();
                                        let row_id = ElementId::Name(
                                            format!("{id_prefix}-{i}").into(),
                                        );
                                        let row_id_for_transition = row_id.clone();
                                        let draggable = name != "." && name != "..";
                                        let drag_remote_path = target_path.clone();
                                        let drag_name = name.clone();
                                        let drag_is_dir = is_dir;
                                        let drag_source_side = side;

                                        div()
                                            .id(row_id.clone())
                                            .h(px(26.0))
                                            .w_full()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1p5()
                                            .px_2()
                                            .rounded(px(4.0))
                                            .when(draggable, |el| {
                                                el.on_drag(
                                                    SftpDragValue {
                                                        remote_path: drag_remote_path.clone(),
                                                        name: drag_name.clone(),
                                                        is_dir: drag_is_dir,
                                                        source_side: drag_source_side,
                                                    },
                                                    |drag_value, _offset, _w, cx| {
                                                        cx.new(|_| drag_value.clone())
                                                    },
                                                )
                                            })
                                            .on_mouse_down(MouseButton::Left, {
                                                let name = name.clone();
                                                let is_dir = is_dir;
                                                let on_navigate = on_navigate.clone();
                                                let on_edit = on_edit.clone();
                                                let target = target_path.clone();
                                                let entity = entity.clone();
                                                move |event, _w, cx| {
                                                    if is_dir && event.click_count == 2 {
                                                        if let Some(ref cb) = on_navigate {
                                                            cb(target.clone(), cx);
                                                        }
                                                        return;
                                                    }
                                                    if !is_dir && event.click_count == 2 && name != ".." && name != "." {
                                                        if let Some(ref cb) = on_edit {
                                                            cb(target.clone(), cx);
                                                        }
                                                        return;
                                                    }
                                                    if name == ".." || name == "." {
                                                        return;
                                                    }
                                                    let _ = entity.update(cx, |view, cx| {
                                                        let panel = view.panel_mut(side);
                                                        if event.modifiers.secondary() {
                                                            if panel.selected.contains(name.as_str()) {
                                                                panel.selected.remove(name.as_str());
                                                            } else {
                                                                panel.selected.insert(name.clone());
                                                            }
                                                        } else {
                                                            panel.selected.clear();
                                                            panel.selected.insert(name.clone());
                                                        }
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .on_mouse_down(MouseButton::Right, {
                                                let name = name.clone();
                                                let target_path = target_path.clone();
                                                let on_navigate = on_navigate.clone();
                                                let on_download = on_download.clone();
                                                let on_delete = on_delete.clone();
                                                let on_rename = on_rename.clone();
                                                let on_edit = on_edit.clone();
                                                let entity = entity.clone();
                                                let alert_controller = alert_controller.clone();
                                                move |event, _w, cx| {
                                                    let Some(ref cm) = context_menu else {
                                                        return;
                                                    };
                                                    let pos = event.position;
                                                    let menu_entries = entity
                                                        .update(cx, |view, cx| -> Vec<(String, bool, String)> {
                                                            let panel = view.panel_mut(side);
                                                            if !panel.selected.contains(name.as_str()) {
                                                                panel.selected.clear();
                                                                if name != ".." && name != "." {
                                                                    panel.selected.insert(name.clone());
                                                                }
                                                            }
                                                            panel.context_menu_entry = Some(name.clone());
                                                            cx.notify();
                                                            let cwd_str = panel
                                                                .remote_cwd
                                                                .as_ref()
                                                                .map(|s| s.as_str())
                                                                .unwrap_or("/");
                                                            panel.remote_entries
                                                                .iter()
                                                                .filter(|e| {
                                                                    e.name != "."
                                                                        && e.name != ".."
                                                                        && panel.selected.contains(e.name.as_str())
                                                                })
                                                                .map(|e| {
                                                                    let p = join_remote_path(cwd_str, &e.name);
                                                                    (e.name.clone(), e.is_dir, p)
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();

                                                    let mut items: Vec<ContextMenuItem> = Vec::new();

                                                    // Enter (single dir).
                                                    if menu_entries.len() == 1 && menu_entries[0].1 {
                                                        let target = menu_entries[0].2.clone();
                                                        let on_navigate = on_navigate.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.enter").to_string(),
                                                            move |_w, cx| {
                                                                if let Some(ref cb) = on_navigate {
                                                                    cb(target.clone(), cx);
                                                                }
                                                            },
                                                        ));
                                                    }

                                                    // Open in Editor (single file).
                                                    if menu_entries.len() == 1 && !menu_entries[0].1 && on_edit.is_some() {
                                                        let remote_path = menu_entries[0].2.clone();
                                                        let on_edit = on_edit.clone();
                                                        let entity_for_clear = entity.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.edit").to_string(),
                                                            move |_w, cx| {
                                                                if let Some(ref cb) = on_edit {
                                                                    cb(remote_path.clone(), cx);
                                                                }
                                                                let _ = entity_for_clear.update(cx, |view, cx| {
                                                                    view.panel_mut(side).selected.clear();
                                                                    cx.notify();
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    // Download.
                                                    if !menu_entries.is_empty() {
                                                        let count = menu_entries.len();
                                                        let label = if count == 1 {
                                                            t!("sftp.download").to_string()
                                                        } else {
                                                            t!("sftp.download_n", count = count).to_string()
                                                        };
                                                        let to_download = menu_entries.clone();
                                                        let on_download = on_download.clone();
                                                        let entity_for_clear = entity.clone();
                                                        // If the other panel is remote,
                                                        // this is remote→remote transfer.
                                                        if other_is_remote && other_on_upload.is_some() {
                                                            let other_on_upload = other_on_upload.clone();
                                                            let other_on_upload_batch = other_on_upload_batch.clone();
                                                            let other_remote_cwd = other_remote_cwd.clone();
                                                            let this_on_download = on_download.clone();
                                                            items.push(ContextMenuItem::new(label, move |_w, cx| {
                                                                let _ = entity_for_clear.update(cx, |view, cx| {
                                                                    view.panel_mut(side).selected.clear();
                                                                    cx.notify();
                                                                });
                                                                // Remote→remote: download to
                                                                // temp, then upload.
                                                                trigger_remote_to_remote_transfer(
                                                                    to_download.clone(),
                                                                    this_on_download.as_ref(),
                                                                    other_on_upload.as_ref(),
                                                                    other_on_upload_batch.as_ref(),
                                                                    other_remote_cwd.as_ref(),
                                                                    cx,
                                                                );
                                                            }));
                                                        } else {
                                                            // Remote→local download
                                                            items.push(ContextMenuItem::new(label, move |_w, cx| {
                                                                if to_download.is_empty() {
                                                                    return;
                                                                }
                                                                let _ = entity_for_clear.update(cx, |view, cx| {
                                                                    view.panel_mut(side).selected.clear();
                                                                    cx.notify();
                                                                });
                                                                trigger_batch_download(
                                                                    to_download.clone(),
                                                                    on_download.as_ref(),
                                                                    cx,
                                                                );
                                                            }));
                                                        }
                                                    }

                                                    // Upload to remote (from other panel's local cwd).
                                                    if !other_is_remote && !other_local_entries.is_empty() && on_upload_for_ctx.is_some() {
                                                        let local_entries = other_local_entries.clone();
                                                        let local_cwd = other_local_cwd.clone();
                                                        let on_upload = on_upload_for_ctx.clone();
                                                        let on_upload_batch = on_upload_batch_for_ctx.clone();
                                                        let remote_cwd = remote_cwd_for_ctx.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.upload").to_string(),
                                                            move |_w, cx| {
                                                                trigger_upload_from_local(
                                                                    local_entries.clone(),
                                                                    local_cwd.clone(),
                                                                    remote_cwd.clone(),
                                                                    on_upload.as_ref(),
                                                                    on_upload_batch.as_ref(),
                                                                    cx,
                                                                );
                                                            },
                                                        ));
                                                    }

                                                    // Rename (single).
                                                    if menu_entries.len() == 1 && name != ".." && on_rename.is_some() {
                                                        let entry_name = menu_entries[0].0.clone();
                                                        let entity_for_rename = entity.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.rename").to_string(),
                                                            move |window, cx| {
                                                                let _ = entity_for_rename.update(cx, |view, cx| {
                                                                    view.start_remote_rename(side, entry_name.clone(), window, cx);
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    // Fallback: enter on "..".
                                                    if items.is_empty() {
                                                        let target = target_path.clone();
                                                        let on_navigate = on_navigate.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.enter").to_string(),
                                                            move |_w, cx| {
                                                                if let Some(ref cb) = on_navigate {
                                                                    cb(target.clone(), cx);
                                                                }
                                                            },
                                                        ));
                                                    }

                                                    // Delete.
                                                    if name != ".." {
                                                        items.push(
                                                            ContextMenuItem::new(t!("sftp.delete").to_string(), {
                                                                let alert_controller = alert_controller.clone();
                                                                let name = name.clone();
                                                                let target_path = target_path.clone();
                                                                let on_delete = on_delete.clone();
                                                                let entity_for_clear = entity.clone();
                                                                move |_w, cx| {
                                                                    let Some(ref ac) = alert_controller else { return };
                                                                    let target_path = target_path.clone();
                                                                    let on_delete = on_delete.clone();
                                                                    let entity_for_clear = entity_for_clear.clone();
                                                                    ac.update(cx, |c, cx| {
                                                                        c.show(
                                                                            AlertState {
                                                                                severity: AlertSeverity::Danger,
                                                                                title: t!("sftp.delete_title").to_string().into(),
                                                                                description: Some(
                                                                                    t!("sftp.delete_prompt", name = name.as_str()).to_string().into(),
                                                                                ),
                                                                                confirm_label: t!("sftp.delete_confirm").to_string().into(),
                                                                                cancel_label: t!("terminal.host_key_cancel").to_string().into(),
                                                                                on_confirm: Some(Rc::new(move |_w, cx| {
                                                                                    if let Some(ref cb) = on_delete {
                                                                                        cb(target_path.clone(), cx);
                                                                                    }
                                                                                    let _ = entity_for_clear.update(cx, |view, cx| {
                                                                                        view.panel_mut(side).selected.clear();
                                                                                        cx.notify();
                                                                                    });
                                                                                })),
                                                                                ..AlertState::default()
                                                                            },
                                                                            cx,
                                                                        );
                                                                    });
                                                                }
                                                            }).danger(true),
                                                        );
                                                    }

                                                    cm.update(cx, |c, cx| {
                                                        c.show(
                                                            ContextMenuState {
                                                                position: pos,
                                                                items,
                                                                ..ContextMenuState::default()
                                                            },
                                                            cx,
                                                        );
                                                    });
                                                }
                                            })
                                            .with_transition(row_id_for_transition)
                                            .on_hover({
                                                let name = name.clone();
                                                move |hovered, _w, cx| {
                                                    let _ = entity.update(cx, |view, cx| {
                                                        let panel = view.panel_mut(side);
                                                        if *hovered {
                                                            panel.hovered = Some(name.clone());
                                                        } else if panel.hovered.as_deref()
                                                            == Some(name.as_str())
                                                        {
                                                            panel.hovered = None;
                                                        }
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .transition_when_else(
                                                is_highlighted,
                                                Duration::from_millis(120),
                                                Linear,
                                                |el| el.bg(rgba((surface_hover() << 8) | 0xFF)),
                                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                                            )
                                            .relative()
                                            .child(
                                                div()
                                                    .id(ElementId::Name(format!("{id_prefix}-bar-{i}").into()))
                                                    .absolute()
                                                    .top(px(2.0))
                                                    .bottom(px(2.0))
                                                    .left_0()
                                                    .w(px(2.0))
                                                    .rounded(px(1.0))
                                                    .bg(rgb(btn_primary_bg()))
                                                    .opacity(0.0)
                                                    .with_transition(ElementId::Name(format!("{id_prefix}-bar-{i}").into()))
                                                    .transition_when_else(
                                                        is_selected,
                                                        Duration::from_millis(120),
                                                        Linear,
                                                        |el| el.opacity(1.0),
                                                        |el| el.opacity(0.0),
                                                    ),
                                            )
                                            .child(
                                                svg()
                                                    .path(icon_path)
                                                    .size(px(14.0))
                                                    .flex_shrink_0()
                                                    .text_color(rgb(text_muted())),
                                            )
                                            .when_some(
                                                if is_renaming { row_rename_input } else { None },
                                                |el, input| {
                                                    el.child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                StyledInput::new(
                                                                    format!("{id_prefix}-rename-{i}"),
                                                                    input,
                                                                ).xsmall(),
                                                            ),
                                                    )
                                                    .child(render_metadata_columns(
                                                        entry_size,
                                                        &entry_permissions,
                                                        entry_modified,
                                                    ))
                                                },
                                            )
                                            .when(!is_renaming, |el| {
                                                el
                                                    .child(
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
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&scroll_handle),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&scroll_handle)
                                    .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
                            ),
                    )
                    .child(
                        DropZoneOverlay::new(drag_over)
                            .hint(t!("sftp.drop_upload_hint").to_string())
                            .id(SharedString::from(format!("{id_prefix}-drop-overlay"))),
                    )
                    .child({
                        let entity = entity_for_list.clone();
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |_bounds, _state, window, _cx| {
                                window.on_mouse_event({
                                    let entity = entity.clone();
                                    move |event: &FileDropEvent, phase, _window, cx| {
                                        if phase != DispatchPhase::Capture {
                                            return;
                                        }
                                        if matches!(event, FileDropEvent::Exited) {
                                            let _ = entity.update(cx, |view, cx| {
                                                let panel = view.panel_mut(side);
                                                if panel.drag_over {
                                                    panel.drag_over = false;
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    }
                                });
                            },
                        )
                        .w_0()
                        .h_0()
                    })
                    // Connection overlay (loading spinner, host-key prompt,
                    // reconnect) — rendered per panel.
                    .when_some(terminal_entity.as_ref(), |el, term| {
                        let (
                            overlay_visible,
                            is_fading_out,
                            log_entries,
                            current_status,
                            spinner_rot,
                        ) = term.read_with(cx, |view, _cx| {
                            let ov = view.overlay_state();
                            let ov = ov.lock();
                            (
                                ov.is_visible(),
                                ov.is_fading_out(),
                                ov.logs.clone(),
                                ov.status,
                                ov.spinner_rotation
                                    .load(std::sync::atomic::Ordering::Relaxed),
                            )
                        });
                        let on_reconnect: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>> =
                            Some(Rc::new({
                                let term = term.clone();
                                move |_e: &ClickEvent, _w: &mut Window, cx: &mut App| {
                                    term.update(cx, |view, cx| {
                                        view.reconnect(cx);
                                    });
                                }
                            }));
                        el.child(
                            crate::views::terminal::connection_overlay::render_connection_overlay(
                                overlay_visible,
                                is_fading_out,
                                current_status,
                                &log_entries,
                                // Encode side into the count so left/right
                                // panels don't share transition IDs.
                                connect_count * 2 + side as u64,
                                spinner_rot,
                                on_reconnect,
                            ),
                        )
                    })
            )
    }
}
