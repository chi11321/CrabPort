//! Panel rendering for `SftpTabView`.
//!
//! A single `render_panel` method handles both left and right panels,
//! each of which can be local or remote. This is a separate `impl` block
//! from the one in [`super::view`]. Rust allows multiple `impl` blocks
//! per type.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::InputState;
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
use super::helpers::render_panel_ellipsis_button;
use super::helpers::{
    trigger_batch_download, trigger_remote_to_remote_transfer, trigger_upload_from_local,
};
use super::pane::{
    build_entry_rows, cmp_dirs_first, cmp_remote_listing, file_row_visuals, join_remote_path,
    open_host_selector_handler, other_side, panel_drag_over_tracker, remote_target_path,
    render_drag_exit_canvas, render_scrollbar_overlay, tab_id_prefix,
};
use super::view::{PanelHost, SftpTabView};

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

/// Render the inline "new folder" input row shown above the file list when
/// the user has triggered "new folder" from the ellipsis menu. The input
/// is pre-seeded with a unique default name by `start_make_folder`; Enter
/// commits, Escape / blur cancels (the latter is wired in `start_make_folder`
/// via `cx.on_blur`).
///
/// Layout matches a single file row (icon + name) so it reads as a pending
/// new entry rather than a foreign floating dialog.
fn render_mkdir_input(id: SharedString, input: Entity<InputState>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("{id}-row")))
        .w_full()
        .h(px(26.0))
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .px_2()
        .bg(rgba((surface_hover() << 8) | 0x55))
        .border_b_1()
        .border_color(rgb(border()))
        // Icon spacer — matches the 14px folder icon in each row.
        .child(
            svg()
                .path("icons/folder.svg")
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(rgb(text_muted())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(StyledInput::new(id, input).xsmall()),
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
        let id_prefix = tab_id_prefix(side);

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
                .on_click(open_host_selector_handler(entity, side)),
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
        let (all_entries, item_sizes) = build_entry_rows(
            panel.local_entries.iter().cloned().collect(),
            cmp_dirs_first,
        );
        let scroll_handle = panel.scroll.clone();
        let local_cwd = panel.local_cwd.clone();
        let path_input = panel.path_input.clone();

        // The "other" panel — for cross-panel operations
        let other_side = other_side(side);
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
        let mkdir_pending = panel.mkdir_pending.is_some();
        let mkdir_input = panel.mkdir_input.clone();

        let id_prefix = tab_id_prefix(side);

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
                            .max_w(px(160.0))
                            .min_w_0()
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
                                    .flex_shrink_0()
                                    .text_color(rgb(text_muted())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_primary()))
                                    .truncate()
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
                    )
                    // Ellipsis overflow menu: refresh / new folder /
                    // toggle hidden / close panel.
                    .child(render_panel_ellipsis_button(
                        side,
                        entity.clone(),
                        self.context_menu.clone(),
                        tooltip_ctrl.clone(),
                        panel.show_hidden,
                        false,
                        None,
                        None,
                        None,
                        None,
                    )),
            )
            // Column header — outside the scroll container so the
            // scrollbar (absolute top_0..bottom_0 inside the container
            // below) only spans the file-list area, not the header.
            .child(render_column_header(id_prefix))
            // Inline "new folder" input — shown above the list when the
            // user has triggered "new folder" from the ellipsis menu.
            // Press Enter to commit, click away / Esc to cancel.
            .when(mkdir_pending, |el| {
                el.when_some(mkdir_input.clone(), |el, input| {
                    el.child(render_mkdir_input(
                        SharedString::from(format!("{id_prefix}-mkdir")),
                        input,
                    ))
                })
            })
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
                    .on_drag_move::<SftpDragValue>(panel_drag_over_tracker(
                        &entity_for_list,
                        side,
                    ))
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
                                        let other_side = super::pane::other_side(side);
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

                                        let row = div()
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
                                                                    let p = join_remote_path(&cwd_str, &e.name);
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
                                                                    view.start_rename(side, entry_name.clone(), false, window, cx);
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
                                            });
                                        file_row_visuals(
                                            row,
                                            row_id_for_transition,
                                            id_prefix,
                                            i,
                                            name,
                                            entity,
                                            side,
                                            is_highlighted,
                                            is_selected,
                                            is_renaming,
                                            row_rename_input,
                                            icon_path,
                                            entry_size,
                                            entry_permissions,
                                            entry_modified,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&scroll_handle),
                    )
                    .child(render_scrollbar_overlay(&scroll_handle))
                    .child(
                        DropZoneOverlay::new(drag_over)
                            .hint(t!("sftp_tab.drop_download_hint").to_string())
                            .id(SharedString::from(format!("{id_prefix}-drop-overlay"))),
                    )
                    // Canvas to catch FileDropEvent::Exited for external drags.
                    .child({
                        let entity = entity_for_list.clone();
                        render_drag_exit_canvas(move |cx| {
                            let _ = entity.update(cx, |view, cx| {
                                let panel = view.panel_mut(side);
                                if panel.drag_over {
                                    panel.drag_over = false;
                                    cx.notify();
                                }
                            });
                        })
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
        let (all_entries, item_sizes) = build_entry_rows(
            panel.remote_entries.iter().cloned().collect(),
            cmp_remote_listing,
        );
        let scroll_handle = panel.scroll.clone();
        let cwd = panel.remote_cwd.clone();
        let path_input = panel.path_input.clone();
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
        let mkdir_pending = panel.mkdir_pending.is_some();
        let mkdir_input = panel.mkdir_input.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();

        // The "other" panel — for remote-to-remote drag transfers.
        let other_side = other_side(side);
        let other_panel = self.panel(other_side);
        let other_is_remote = other_panel.host.is_remote();
        let other_on_download_for_r2r = other_panel.on_download.clone();

        let host_label = match &panel.host {
            PanelHost::Remote { host_name, .. } => host_name.clone(),
            _ => t!("sftp_tab.no_host").to_string(),
        };

        // The terminal for rendering (zero-size, keeps frame pump alive)
        let terminal_entity = panel.host.terminal().cloned();

        let id_prefix = tab_id_prefix(side);

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
                            .max_w(px(160.0))
                            .min_w_0()
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
                                    .flex_shrink_0()
                                    .text_color(rgb(text_muted())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_primary()))
                                    .truncate()
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
                    )
                    // Ellipsis overflow menu: download / upload / refresh /
                    // new folder / toggle hidden / close panel.
                    .child(render_panel_ellipsis_button(
                        side,
                        entity.clone(),
                        self.context_menu.clone(),
                        tooltip_ctrl.clone(),
                        panel.show_hidden,
                        true,
                        on_download.clone(),
                        on_upload.clone(),
                        on_upload_batch.clone(),
                        cwd.clone(),
                    )),
            )
            // Column header — outside the scroll container so the
            // scrollbar (absolute top_0..bottom_0 inside the container
            // below) only spans the file-list area, not the header.
            .child(render_column_header(id_prefix))
            // Inline "new folder" input — shown above the list when the
            // user has triggered "new folder" from the ellipsis menu.
            // Press Enter to commit, click away / Esc to cancel.
            .when(mkdir_pending, |el| {
                el.when_some(mkdir_input.clone(), |el, input| {
                    el.child(render_mkdir_input(
                        SharedString::from(format!("{id_prefix}-mkdir")),
                        input,
                    ))
                })
            })
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
                    .on_drag_move::<LocalFileDragValue>(panel_drag_over_tracker(
                        &entity_for_list,
                        side,
                    ))
                    .on_drag_move::<ExternalPaths>(panel_drag_over_tracker(
                        &entity_for_list,
                        side,
                    ))
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
                    .on_drag_move::<SftpDragValue>(panel_drag_over_tracker(
                        &entity_for_list,
                        side,
                    ))
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
                                        let target_path = remote_target_path(cwd_ref, &name);

                                        let on_navigate = this.panel(side).on_navigate.clone();
                                        let on_download = this.panel(side).on_download.clone();
                                        let on_delete = this.panel(side).on_delete.clone();
                                        let on_rename = this.panel(side).on_rename.clone();
                                        let on_edit = this.panel(side).on_edit.clone();
                                        let on_upload_for_ctx = this.panel(side).on_upload.clone();
                                        let on_upload_batch_for_ctx = this.panel(side).on_upload_batch.clone();
                                        let remote_cwd_for_ctx = this.panel(side).remote_cwd.clone();

                                        // The "other" panel's state for cross-panel ops
                                        let other_side = super::pane::other_side(side);
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

                                        let row = div()
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
                                                                    view.start_rename(side, entry_name.clone(), true, window, cx);
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
                                            });
                                        file_row_visuals(
                                            row,
                                            row_id_for_transition,
                                            id_prefix,
                                            i,
                                            name,
                                            entity,
                                            side,
                                            is_highlighted,
                                            is_selected,
                                            is_renaming,
                                            row_rename_input,
                                            icon_path,
                                            entry_size,
                                            entry_permissions,
                                            entry_modified,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&scroll_handle),
                    )
                    .child(render_scrollbar_overlay(&scroll_handle))
                    .child(
                        DropZoneOverlay::new(drag_over)
                            .hint(t!("sftp.drop_upload_hint").to_string())
                            .id(SharedString::from(format!("{id_prefix}-drop-overlay"))),
                    )
                    .child({
                        let entity = entity_for_list.clone();
                        render_drag_exit_canvas(move |cx| {
                            let _ = entity.update(cx, |view, cx| {
                                let panel = view.panel_mut(side);
                                if panel.drag_over {
                                    panel.drag_over = false;
                                    cx.notify();
                                }
                            });
                        })
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
                                // panels don't share transition IDs. The
                                // `100_000` offset keeps SFTP panel overlay
                                // IDs out of the SSH tab pane-id space
                                // (pane IDs start at 1 and grow slowly);
                                // without this, an SFTP panel's overlay
                                // transition ID would collide with an SSH
                                // tab's, and whichever rendered second
                                // would inherit the other's cached
                                // transition state (e.g. "already faded
                                // out"), making the overlay invisible.
                                100_000 + connect_count * 2 + side as u64,
                                spinner_rot,
                                on_reconnect,
                            ),
                        )
                    })
            )
    }
}
