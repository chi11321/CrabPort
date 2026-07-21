//! Free helper functions for the SFTP tab view: the ellipsis (overflow)
//! menu button rendering and the batch download / upload orchestration flows.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crate::color::*;
use crate::components::context_menu::{ContextMenuItem, ContextMenuState};
use crate::motion::{EASE_STANDARD, duration_fast};

use super::view::{SftpTabView, join_remote_path};
use crate::components::host_selector::PanelSide;

// ---------------------------------------------------------------------------
// Panel ellipsis (overflow) menu button
// ---------------------------------------------------------------------------

/// Render the ellipsis button shown at the right of a panel's path bar.
///
/// Clicking it opens a [`ContextMenuController`] anchored to the click
/// position with these actions: download (remote only) / upload (remote
/// only) / refresh / new folder / toggle hidden files / close
/// (disconnect). The menu is non-sticky — every item click both invokes
/// its handler and dismisses the menu. The "show hidden files" item is
/// labelled to reflect the *current* state so the user can tell at a
/// glance whether hidden files are visible.
///
/// `show_hidden` is the panel's current state, read by the caller (which
/// has `&SftpTabView`). We take it as a param rather than reading inside
/// the element builder because the panel borrow isn't available at click
/// time.
///
/// `on_download` / `on_upload` / `on_upload_batch` / `cwd` are only
/// `Some` for remote panels — local panels don't show the download/upload
/// items. They're threaded through here rather than re-read at click time
/// because they're cheap `Rc` clones captured per-render.
pub(super) fn render_panel_ellipsis_button(
    side: PanelSide,
    entity: WeakEntity<SftpTabView>,
    context_menu: Option<Entity<crate::components::context_menu::ContextMenuController>>,
    tooltip_ctrl: Option<Entity<crate::components::tooltip::TooltipController>>,
    show_hidden: bool,
    is_remote: bool,
    on_download: Option<Rc<dyn Fn(String, String, &mut App)>>,
    on_upload: Option<Rc<dyn Fn(String, String, &mut App)>>,
    on_upload_batch: Option<Rc<dyn Fn(Vec<(String, String)>, &mut App)>>,
    cwd: Option<Arc<String>>,
) -> impl IntoElement {
    let id_prefix = match side {
        PanelSide::Left => "sftp-tab-left",
        PanelSide::Right => "sftp-tab-right",
    };
    let btn_id = ElementId::Name(format!("{id_prefix}-ellipsis-btn").into());
    // Resting background is a faint surface tint (alpha ~30%) rather than
    // fully transparent — the panel reads better with a visible button.
    let rest_bg = rgba((surface_hover() << 8) | 0x33);
    let hover_bg_rgba = rgba((surface_hover() << 8) | 0xFF);
    let tooltip_text = t!("sftp_tab.actions").to_string();

    div()
        .id(btn_id.clone())
        .flex()
        .items_center()
        .justify_center()
        .size(px(26.0))
        .flex_shrink_0()
        .rounded(px(4.0))
        .bg(rest_bg)
        .with_transition(btn_id)
        .transition_on_hover(duration_fast(), EASE_STANDARD, move |hovered, el| {
            if *hovered {
                el.bg(hover_bg_rgba)
            } else {
                el.bg(rest_bg)
            }
        })
        .when_some(tooltip_ctrl.clone(), |el, ctrl| {
            el.on_hover(move |hovered, w, cx| {
                if *hovered {
                    ctrl.update(cx, |t, cx| {
                        t.show(tooltip_text.clone(), w.mouse_position(), cx);
                    });
                } else {
                    ctrl.update(cx, |t, cx| {
                        t.hide(cx);
                    });
                }
            })
        })
        .on_click(move |e, _w, cx| {
            let Some(ref cm) = context_menu else {
                return;
            };
            // Resolve the current show_hidden state at click time so the
            // menu reflects the latest toggle (the user may have changed
            // it since the last render).
            let cur_show_hidden = entity
                .read_with(cx, |view, _cx| view.panel(side).show_hidden)
                .unwrap_or(show_hidden);
            let cur_hidden_label = if cur_show_hidden {
                t!("sftp_tab.hide_hidden").to_string()
            } else {
                t!("sftp_tab.show_hidden").to_string()
            };
            let refresh_label = t!("sftp.refresh").to_string();
            let mkdir_label = t!("sftp_tab.mkdir").to_string();
            let close_label = t!("sftp_tab.close_panel").to_string();
            let download_label = t!("sftp.download").to_string();
            let upload_label = t!("sftp.upload").to_string();

            let mut items: Vec<ContextMenuItem> = Vec::new();

            // Download / upload items are only meaningful for remote panels.
            if is_remote {
                if on_download.is_some() {
                    let entity = entity.clone();
                    let on_download = on_download.clone();
                    let cwd = cwd.clone();
                    items.push(
                        ContextMenuItem::new(download_label.clone(), move |_w, cx| {
                            trigger_remote_download_from_button(
                                entity.clone(),
                                side,
                                on_download.as_ref(),
                                cwd.as_ref(),
                                cx,
                            );
                        })
                        .with_icon("icons/download.svg"),
                    );
                }
                if on_upload.is_some() {
                    let entity = entity.clone();
                    let on_upload = on_upload.clone();
                    let on_upload_batch = on_upload_batch.clone();
                    let cwd = cwd.clone();
                    items.push(
                        ContextMenuItem::new(upload_label.clone(), move |_w, cx| {
                            trigger_upload(
                                entity.clone(),
                                side,
                                on_upload.as_ref(),
                                on_upload_batch.as_ref(),
                                cwd.as_ref(),
                                cx,
                            );
                        })
                        .with_icon("icons/upload.svg"),
                    );
                }
                // If we added transfer items, separate them from the
                // filesystem actions with a divider.
                if !items.is_empty() {
                    items.last_mut().unwrap().divider_after = true;
                }
            }

            items.push(
                ContextMenuItem::new(refresh_label.clone(), {
                    let entity = entity.clone();
                    move |_w, cx| {
                        let _ = entity.update(cx, |view, cx| {
                            view.refresh_listing(side, cx);
                        });
                    }
                })
                .with_icon("icons/refresh-cw.svg"),
            );
            items.push(
                ContextMenuItem::new(mkdir_label.clone(), {
                    let entity = entity.clone();
                    move |w, cx| {
                        let _ = entity.update(cx, |view, cx| {
                            view.start_make_folder(side, w, cx);
                        });
                    }
                })
                .with_icon("icons/plus.svg"),
            );
            items.push(
                ContextMenuItem::new(cur_hidden_label, {
                    let entity = entity.clone();
                    move |_w, cx| {
                        let _ = entity.update(cx, |view, cx| {
                            view.toggle_hidden(side, cx);
                        });
                    }
                })
                .with_icon("icons/eye.svg"),
            );
            // Close is the destructive action — separate + style it.
            items.last_mut().unwrap().divider_after = true;
            items.push(
                ContextMenuItem::new(close_label.clone(), {
                    let entity = entity.clone();
                    move |w, cx| {
                        let _ = entity.update(cx, |view, cx| {
                            view.disconnect_panel(side, w, cx);
                        });
                    }
                })
                .with_icon("icons/close.svg")
                .danger(true),
            );

            let state = ContextMenuState {
                position: e.position(),
                items,
                header: None,
                open: false,
                sticky: false,
            };
            cm.update(cx, |c, cx| {
                c.show(state, cx);
            });
            cx.stop_propagation();
        })
        .child(
            svg()
                .path("icons/ellipsis.svg")
                .size(px(14.0))
                .text_color(rgb(text_muted())),
        )
}
// ---------------------------------------------------------------------------

/// Drive a batch SFTP download (re-uses the side-panel flow).
pub(super) fn trigger_batch_download(
    entries: Vec<(String, bool, String)>,
    on_download: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    cx: &mut App,
) {
    let Some(on_download) = on_download else {
        return;
    };
    let on_download = on_download.clone();

    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t!("sftp.download_prompt").to_string().into()),
    });

    cx.spawn(async move |cx| {
        let picked = match rx.await {
            Ok(Ok(Some(mut paths))) => paths.pop(),
            Ok(Ok(None)) => {
                tracing::info!("SFTP tab download: user cancelled folder picker");
                None
            }
            Ok(Err(e)) => {
                tracing::warn!("SFTP tab download: folder picker error: {e}");
                None
            }
            Err(e) => {
                tracing::warn!("SFTP tab download: picker channel closed: {e}");
                None
            }
        };
        let Some(dest_dir) = picked else { return };
        let _ = cx.update(|cx| {
            for (name, _is_dir, remote_path) in &entries {
                let local_path = dest_dir.join(name);
                on_download(
                    remote_path.clone(),
                    local_path.to_string_lossy().into_owned(),
                    cx,
                );
            }
        });
    })
    .detach();
}

/// Remote-to-remote transfer: download from source remote to a local
/// temp directory, wait for the download to finish, then upload from
/// temp to the destination remote.
///
/// The `on_download` / `on_upload` callbacks are fire-and-forget — they
/// trigger background SFTP transfers on the respective `TerminalView`'s
/// backend but don't await completion. So we poll the local temp file's
/// size until it stabilises (no growth for 500ms) before uploading.
pub(super) fn trigger_remote_to_remote_transfer(
    entries: Vec<(String, bool, String)>,
    on_download: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    on_upload: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    on_upload_batch: Option<&Rc<dyn Fn(Vec<(String, String)>, &mut App)>>,
    dest_remote_cwd: Option<&Arc<String>>,
    cx: &mut App,
) {
    let Some(on_download) = on_download else {
        tracing::warn!("remote-to-remote: no download callback");
        return;
    };
    let Some(on_upload) = on_upload else {
        tracing::warn!("remote-to-remote: no upload callback");
        return;
    };
    let Some(dest_cwd) = dest_remote_cwd else {
        tracing::warn!("remote-to-remote: no dest remote cwd");
        return;
    };

    let on_download = on_download.clone();
    let on_upload = on_upload.clone();
    let on_upload_batch = on_upload_batch.cloned();
    let dest_cwd = dest_cwd.as_str().to_string();
    let entries = entries.clone();

    cx.spawn(async move |cx| {
        // Create a temp directory for staging.
        let tmp_dir = std::env::temp_dir().join(format!(
            "crabport-r2r-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            tracing::warn!("remote-to-remote: failed to create temp dir: {e}");
            return;
        }

        // Download each entry to temp.
        let _ = cx.update(|cx| {
            for (name, _is_dir, remote_path) in &entries {
                let local_temp = tmp_dir.join(name);
                on_download(
                    remote_path.clone(),
                    local_temp.to_string_lossy().into_owned(),
                    cx,
                );
            }
        });

        // Wait for each downloaded file to finish by polling its size.
        // A file is considered complete when its size hasn't changed for
        // 500ms (avoids busy-waiting while the transfer is active).
        for (name, _is_dir, _remote_path) in &entries {
            let local_temp = tmp_dir.join(name);
            let mut last_size: u64 = 0;
            let mut stable_count: u32 = 0;
            // Timeout after 5 minutes to avoid hanging forever.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                if std::time::Instant::now() > deadline {
                    tracing::warn!("remote-to-remote: timed out waiting for {name}");
                    break;
                }
                let cur_size = std::fs::metadata(&local_temp).map(|m| m.len()).unwrap_or(0);
                if cur_size > 0 && cur_size == last_size {
                    stable_count += 1;
                    if stable_count >= 3 {
                        // 3 × 200ms = 600ms of stability → done.
                        break;
                    }
                } else {
                    stable_count = 0;
                }
                last_size = cur_size;
                smol::Timer::after(std::time::Duration::from_millis(200)).await;
            }
        }

        // Upload each entry from temp to the destination remote.
        let _ = cx.update(|cx| {
            let items: Vec<(String, String)> = entries
                .iter()
                .map(|(name, _is_dir, _)| {
                    let local_temp = tmp_dir.join(name);
                    let remote_dest = join_remote_path(&dest_cwd, name);
                    (local_temp.to_string_lossy().into_owned(), remote_dest)
                })
                .collect();
            if items.is_empty() {
                return;
            }
            if items.len() == 1 {
                on_upload(items[0].0.clone(), items[0].1.clone(), cx);
            } else if let Some(ref cb) = on_upload_batch {
                cb(items, cx);
            } else {
                for (local, remote) in &items {
                    on_upload(local.clone(), remote.clone(), cx);
                }
            }
        });

        // Clean up temp directory after a delay (let uploads start).
        smol::Timer::after(std::time::Duration::from_secs(30)).await;
        let _ = std::fs::remove_dir_all(&tmp_dir);
    })
    .detach();
}

/// Download button handler for the remote panel: collect the current
/// multi-selection and run the batch download flow.
pub(super) fn trigger_remote_download_from_button(
    entity: WeakEntity<SftpTabView>,
    side: PanelSide,
    on_download: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    cwd: Option<&Arc<String>>,
    cx: &mut App,
) {
    let entries = entity.read_with(cx, |view, _cx| {
        let panel = view.panel(side);
        let cwd_str = panel.remote_cwd.as_ref().map(|s| s.as_str()).unwrap_or("/");
        panel
            .remote_entries
            .iter()
            .filter(|e| e.name != "." && e.name != ".." && panel.selected.contains(e.name.as_str()))
            .map(|e| {
                let p = join_remote_path(cwd_str, &e.name);
                (e.name.clone(), e.is_dir, p)
            })
            .collect::<Vec<_>>()
    });
    let Ok(entries) = entries else { return };
    if entries.is_empty() {
        return;
    }
    let _ = entity.update(cx, |view, cx| {
        view.panel_mut(side).selected.clear();
        cx.notify();
    });
    let _ = cwd;
    trigger_batch_download(entries, on_download, cx);
}

/// Upload button handler: open a native multi-select file picker and
/// upload each chosen file into the current remote cwd.
pub(super) fn trigger_upload(
    entity: WeakEntity<SftpTabView>,
    side: PanelSide,
    on_upload: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    on_upload_batch: Option<&Rc<dyn Fn(Vec<(String, String)>, &mut App)>>,
    cwd: Option<&Arc<String>>,
    cx: &mut App,
) {
    let Some(on_upload) = on_upload else { return };
    let on_upload = on_upload.clone();
    let on_upload_batch = on_upload_batch.cloned();
    let Some(cwd) = cwd else { return };
    let cwd = cwd.as_str().to_string();

    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: true,
        prompt: Some(t!("sftp.upload_prompt").to_string().into()),
    });

    cx.spawn(async move |cx| {
        let picked = match rx.await {
            Ok(Ok(Some(paths))) => Some(paths),
            Ok(Ok(None)) => None,
            Ok(Err(_e)) => None,
            Err(_e) => None,
        };
        let Some(paths) = picked else { return };
        if paths.is_empty() {
            return;
        }

        let _ = entity.update(cx, |view, cx| {
            view.panel_mut(side).selected.clear();
            cx.notify();
        });

        let _ = cx.update(|cx| {
            let items: Vec<(String, String)> = paths
                .iter()
                .map(|local| {
                    let name = local
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| local.to_string_lossy().into_owned());
                    let remote = join_remote_path(&cwd, &name);
                    (local.to_string_lossy().into_owned(), remote)
                })
                .collect();
            if items.len() == 1 {
                // Single file: use per-file upload (gzip staging is already
                // efficient for single files).
                on_upload(items[0].0.clone(), items[0].1.clone(), cx);
            } else if let Some(ref cb) = on_upload_batch {
                cb(items, cx);
            } else {
                // No batch callback: fall back to per-file.
                for (local, remote) in &items {
                    on_upload(local.clone(), remote.clone(), cx);
                }
            }
        });
    })
    .detach();
}

/// Upload from another panel's current listing (used by the remote
/// panel's context-menu "Upload" item when the other panel is local).
pub(super) fn trigger_upload_from_local(
    local_entries: Vec<crabport_sftp::FileEntry>,
    local_cwd: PathBuf,
    remote_cwd: Option<Arc<String>>,
    on_upload: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    on_upload_batch: Option<&Rc<dyn Fn(Vec<(String, String)>, &mut App)>>,
    cx: &mut App,
) {
    let Some(on_upload) = on_upload else { return };
    let Some(remote_cwd) = remote_cwd else { return };
    let remote_cwd = remote_cwd.as_str().to_string();

    let items: Vec<(String, String)> = local_entries
        .iter()
        .map(|entry| {
            let local_path = local_cwd.join(&entry.name);
            let remote = join_remote_path(&remote_cwd, &entry.name);
            (local_path.to_string_lossy().into_owned(), remote)
        })
        .collect();

    if items.is_empty() {
        return;
    }

    if items.len() == 1 {
        on_upload(items[0].0.clone(), items[0].1.clone(), cx);
    } else if let Some(cb) = on_upload_batch {
        cb(items, cx);
    } else {
        for (local, remote) in &items {
            on_upload(local.clone(), remote.clone(), cx);
        }
    }
}
