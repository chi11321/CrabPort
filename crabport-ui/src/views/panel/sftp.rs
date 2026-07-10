use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::scroll::Scrollbar;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;
use rustc_hash::FxHashSet;

use crate::color::*;
use crate::components::context_menu::{ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::components::drop_zone_overlay::DropZoneOverlay;
use crate::components::input::StyledInput;

/// Drag payload for an SFTP row being dragged within the app.
/// Dropped onto a terminal area to trigger a download.
#[derive(Clone, Debug)]
pub struct SftpDragValue {
    pub remote_path: String,
    pub name: String,
    pub is_dir: bool,
}

impl Render for SftpDragValue {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A small floating chip showing the file name + icon.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(rgb(bg_base()))
            .border_1()
            .border_color(rgb(border()))
            .shadow_sm()
            .child(
                svg()
                    .path(if self.is_dir {
                        "icons/folder.svg"
                    } else {
                        "icons/file.svg"
                    })
                    .size_3()
                    .text_color(rgb(text_muted())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_primary()))
                    .child(self.name.clone()),
            )
    }
}

/// SFTP panel view.
///
/// Holds its own `InputState` for the path input bar so the typed text
/// persists across renders. Entries / cwd / navigation callback are pushed
/// in from the active terminal's backend before each render via `set_state`.
pub struct SftpPanel {
    /// Path input state. Lazily initialized on the first `set_state` call
    /// (which receives a `&mut Window` required by `InputState::new`).
    path_input: Option<Entity<InputState>>,
    /// Current working directory, shown as the input's default value.
    cwd: Option<Arc<String>>,
    /// Current directory entries.
    entries: Arc<Vec<(String, bool)>>,
    /// Navigate callback — invoked with the typed path on Enter.
    on_navigate: Option<Rc<dyn Fn(String, &mut App)>>,
    /// The tab id whose state is currently reflected in the input.
    /// When the active tab changes we force-sync the input to the new
    /// backend's cwd instead of preserving the previous tab's text.
    active_tab_id: Option<u64>,
    /// Global context menu host. Held so the panel can open a right-click
    /// menu on entries ("Enter" for dirs, "Download" for files).
    context_menu: Option<Entity<crate::components::context_menu::ContextMenuController>>,
    /// Global alert dialog host, used for the delete-confirmation prompt.
    alert_controller: Option<Entity<AlertController>>,
    /// The entry name that triggered the currently-open context menu, if
    /// any. While set, that row stays highlighted in the hover color even
    /// though the mouse has moved to the overlay.
    context_menu_entry: Option<String>,
    /// The entry currently being hovered, if any. Used to drive the hover
    /// background transition (same pattern as SessionsView).
    hovered_entry: Option<String>,
    /// Multi-selection set. Keyed by entry name (unique within the current
    /// cwd listing). `.` and `..` are never added — they're navigation
    /// helpers, not selectable items. Cleared whenever the entries list
    /// changes (e.g. directory navigation) so stale name→path mappings
    /// can't survive a context switch.
    selected: FxHashSet<String>,
    /// Download callback — invoked with `(remote_path, local_path)` for
    /// each entry the user chose to download. Mirrors `on_navigate`'s
    /// signature shape; injected from `content.rs` so this view stays
    /// agnostic of the terminal/backend wiring.
    on_download: Option<Rc<dyn Fn(String, String, &mut App)>>,
    /// Upload callback — invoked with `(local_path, remote_path)` for each
    /// file the user picked. Mirrors `on_download` but with the argument
    /// order swapped to match `view.sftp_upload(local, remote)`.
    on_upload: Option<Rc<dyn Fn(String, String, &mut App)>>,
    /// Delete callback — invoked with the remote path to remove. The
    /// backend stats the path to decide between file/dir removal.
    on_delete: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Rename callback — invoked with `(old_remote_path, new_remote_path)`.
    on_rename: Option<Rc<dyn Fn(String, String, &mut App)>>,
    /// Edit callback — invoked with the remote path to download + open locally.
    on_edit: Option<Rc<dyn Fn(String, &mut App)>>,
    /// When `Some`, a small inline rename-prompt overlay is rendered over the
    /// panel. Holds the entry name being renamed (used as the dialog title
    /// and to resolve the old remote path from the current cwd).
    renaming_entry: Option<String>,
    /// `InputState` backing the rename-prompt overlay. Lazily created the
    /// first time the user triggers a rename; reused thereafter.
    rename_input: Option<Entity<InputState>>,
    /// Scroll handle for the virtual list. Doubles as the handle for the
    /// custom `Scrollbar::vertical` overlay so the scrollbar style stays
    /// consistent with the rest of the app.
    scroll_handle: VirtualListScrollHandle,
    /// Whether external files are currently being dragged over this panel.
    /// Drives a highlighted drop-zone border so the user knows a drop will
    /// upload.
    drag_over: bool,
}

impl SftpPanel {
    pub fn new() -> Self {
        Self {
            path_input: None,
            cwd: None,
            entries: Arc::new(Vec::new()),
            on_navigate: None,
            active_tab_id: None,
            context_menu: None,
            alert_controller: None,
            context_menu_entry: None,
            hovered_entry: None,
            selected: FxHashSet::default(),
            on_download: None,
            on_upload: None,
            on_delete: None,
            on_rename: None,
            on_edit: None,
            renaming_entry: None,
            rename_input: None,
            scroll_handle: VirtualListScrollHandle::new(),
            drag_over: false,
        }
    }

    /// Update the SFTP state from the active terminal's backend.
    /// Called by the content layout each render.
    pub fn set_state(
        &mut self,
        entries: Arc<Vec<(String, bool)>>,
        cwd: Option<Arc<String>>,
        on_navigate: Option<Rc<dyn Fn(String, &mut App)>>,
        on_download: Option<Rc<dyn Fn(String, String, &mut App)>>,
        on_upload: Option<Rc<dyn Fn(String, String, &mut App)>>,
        on_delete: Option<Rc<dyn Fn(String, &mut App)>>,
        on_rename: Option<Rc<dyn Fn(String, String, &mut App)>>,
        on_edit: Option<Rc<dyn Fn(String, &mut App)>>,
        active_tab_id: u64,
        context_menu: Entity<crate::components::context_menu::ContextMenuController>,
        alert_controller: Entity<AlertController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Did the active tab change since last render? This is the signal
        // for a context switch (e.g. user opened another SSH connection),
        // and we must force-sync the input to the new backend's cwd —
        // otherwise the input would keep showing the previous tab's path.
        let tab_changed = self.active_tab_id != Some(active_tab_id);

        // Lazily init the InputState now that we have a Window.
        if self.path_input.is_none() {
            let initial = cwd.as_ref().map(|s| s.as_str()).unwrap_or("/").to_string();
            let entity = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(initial)
                    .placeholder("/path/to/dir")
            });
            // Listen for Enter key to submit navigation.
            cx.subscribe(
                &entity,
                |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        let path = input.read(cx).value().to_string();
                        if !path.is_empty() {
                            if let Some(ref cb) = this.on_navigate {
                                let cb = cb.clone();
                                cx.defer(move |cx| cb(path, cx));
                            }
                        }
                    }
                },
            )
            .detach();

            // On blur, discard any in-progress edit and snap the input back
            // to the backend's current cwd. This avoids stale text lingering
            // when the user clicks away mid-edit.
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, move |this, window, cx| {
                if let Some(ref input) = this.path_input {
                    let value = this
                        .cwd
                        .as_ref()
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "/".to_string());
                    input.update(cx, |state, cx| {
                        state.set_value(value, window, cx);
                    });
                }
            })
            .detach();

            self.path_input = Some(entity);
            self.cwd = cwd.clone();
            self.active_tab_id = Some(active_tab_id);
            self.context_menu = Some(context_menu);
            self.alert_controller = Some(alert_controller);
            self.on_download = on_download;
            self.on_upload = on_upload;
            self.on_delete = on_delete;
            self.on_rename = on_rename;
            self.on_edit = on_edit;
            return;
        }

        // If the backend's cwd changed (e.g. user double-clicked a folder),
        // sync the input to reflect it. Three cases:
        //   1. Tab switched → always overwrite (context switch).
        //   2. Same tab, cwd just arrived (None → Some) → overwrite. This
        //      happens right after connecting when the backend reports its
        //      initial cwd; the `cur == prev` guard below would fail because
        //      `prev` (the old None) serializes to "".
        //   3. Same tab, cwd changed (Some → Some) → only when the input
        //      still shows the previous cwd, so we don't clobber the user's
        //      in-progress edit.
        let cwd_changed = self.cwd.as_ref().map(|s| s.as_str()) != cwd.as_ref().map(|s| s.as_str());
        let cwd_just_arrived = self.cwd.is_none() && cwd.is_some();
        if tab_changed || cwd_changed {
            if let Some(ref input) = self.path_input {
                let should_overwrite = if tab_changed || cwd_just_arrived {
                    true
                } else {
                    let cur = input.read(cx).value().to_string();
                    let prev = self.cwd.as_ref().map(|s| s.as_str()).unwrap_or("");
                    cur == prev
                };
                if should_overwrite {
                    let value = cwd
                        .as_ref()
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "/".to_string());
                    input.update(cx, |state, cx| {
                        state.set_value(value, window, cx);
                    });
                }
            }
        }

        self.cwd = cwd;
        // Detect listing changes so we can invalidate the multi-selection:
        // a name-keyed selection can't safely survive navigation or a refresh
        // because the same name may map to a different remote path, or may
        // no longer exist at all. We compare by reference identity first
        // (cheap — `Arc` pointer eq covers the common case where the backend
        // handed us the same snapshot twice) and fall back to a per-name
        // comparison of the entry list.
        let entries_changed = if Arc::ptr_eq(&self.entries, &entries) {
            false
        } else {
            let prev = self
                .entries
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>();
            let next = entries.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>();
            prev != next
        };
        self.entries = entries;
        self.on_navigate = on_navigate;
        self.on_download = on_download;
        self.on_upload = on_upload;
        self.on_delete = on_delete;
        self.on_rename = on_rename;
        self.on_edit = on_edit;
        self.active_tab_id = Some(active_tab_id);
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        if tab_changed || entries_changed {
            self.selected.clear();
            self.renaming_entry = None;
        }
    }

    /// Begin renaming `entry_name`: stash the name, (re)seed the inline
    /// rename `InputState` with the current name, and focus it. Called from
    /// the context-menu callback, which receives a `&mut Window`.
    fn start_rename(&mut self, entry_name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming_entry = Some(entry_name.clone());
        // Lazily create the InputState the first time.
        if self.rename_input.is_none() {
            let entity = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder("new name");
                state.focus(window, cx);
                state
            });
            // Submit on Enter, cancel on Escape.
            cx.subscribe(
                &entity,
                |this, _input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        this.commit_rename(cx);
                    }
                },
            )
            .detach();
            // Cancel on blur — the user clicked away.
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, |this, _window, cx| {
                if this.renaming_entry.is_some() {
                    this.cancel_rename(cx);
                }
            })
            .detach();
            self.rename_input = Some(entity);
        }
        // Seed the input with the entry's current name.
        if let Some(ref input) = self.rename_input {
            input.update(cx, |state, cx| {
                state.set_value(entry_name, window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Commit the rename: read the new name from `rename_input`, join it
    /// onto the current cwd to form the new remote path, invoke `on_rename`,
    /// and close the overlay.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let new_name = self.rename_input.as_ref().and_then(|input| {
            let v = input.read(cx).value().to_string();
            if v.is_empty() { None } else { Some(v) }
        });
        let Some(new_name) = new_name else {
            return;
        };
        let Some(entry_name) = self.renaming_entry.take() else {
            return;
        };
        // If the name hasn't changed, don't send a rename request — just
        // close the inline editor.
        if new_name == entry_name {
            self.selected.clear();
            cx.notify();
            return;
        }
        let cwd_str = self
            .cwd
            .as_ref()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        let old_path = if cwd_str.ends_with('/') {
            format!("{}{}", cwd_str, entry_name)
        } else {
            format!("{}/{}", cwd_str, entry_name)
        };
        let new_path = if cwd_str.ends_with('/') {
            format!("{}{}", cwd_str, new_name)
        } else {
            format!("{}/{}", cwd_str, new_name)
        };
        if let Some(ref cb) = self.on_rename {
            let cb = cb.clone();
            let old = old_path.clone();
            let new = new_path.clone();
            cx.defer(move |cx| cb(old, new, cx));
        }
        // Clear the selection so the renamed row's highlight doesn't linger.
        self.selected.clear();
        cx.notify();
    }

    /// Abort the rename overlay without invoking the callback.
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming_entry = None;
        cx.notify();
    }
}

impl Render for SftpPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Sort entries alphabetically, directories first
        let mut sorted: Vec<(String, bool)> = self.entries.iter().cloned().collect();
        sorted.sort_by(|a, b| match (a.0.as_str(), b.0.as_str()) {
            (".", _) => std::cmp::Ordering::Less,
            (_, ".") => std::cmp::Ordering::Greater,
            ("..", _) => std::cmp::Ordering::Less,
            (_, "..") => std::cmp::Ordering::Greater,
            _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
        });

        // Prepend .. entry
        let mut all_entries: Vec<(String, bool)> = vec![("..".into(), true)];
        all_entries.extend(sorted);

        let path_input = self.path_input.clone();
        let entity = _cx.entity().downgrade();

        // The inline rename InputState is created in `start_rename` (which has
        // a `&mut Window`). Here we just clone the Entity so row closures can
        // render it in place of the label when `renaming_entry` matches.
        let renaming_entry = self.renaming_entry.clone();
        let rename_input = self.rename_input.clone();

        // If the global context menu is no longer active, clear the
        // "menu-triggering entry" highlight.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(_cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.context_menu_entry = None;
        }

        // Pre-compute item sizes for the virtual list. All rows share a
        // fixed height (26px); width is left at 0 so VirtualList uses the
        // container width. This satisfies the "precompute item sizes"
        // best practice — the list never has to measure rows at runtime.
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
        let scroll_handle = self.scroll_handle.clone();

        // Clone the action-button callbacks out of `self` so the button
        // row closures (built below) can capture them by move. The virtual
        // list's render closure reads `self` directly via its `&mut SftpPanel`
        // argument, so it doesn't need these snapshots.
        let cwd = self.cwd.clone();
        let on_navigate = self.on_navigate.clone();
        let on_download = self.on_download.clone();
        let on_upload = self.on_upload.clone();
        let on_upload_for_drop = on_upload.clone();
        let cwd_for_drop = cwd.clone();
        let drag_over = self.drag_over;
        let entity_for_drop = _cx.entity().downgrade();
        let entity_for_list = entity_for_drop.clone();

        div()
            .h_full()
            .w_full()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .pt_1()
            .px_1()
            .relative()
            .when_some(path_input, |el, input| {
                el.child(
                    div().mb_1().child(
                        StyledInput::new("sftp-path", input).xsmall().prefix(
                            svg()
                                .path("icons/folder.svg")
                                .size(px(12.0))
                                .text_color(rgb(text_muted())),
                        ),
                    ),
                )
            })
            // Action button row: upload / download / refresh. Compact
            // icon-only buttons that sit between the path bar and the
            // listing. Upload opens a native file picker (multi-select) and
            // uploads each chosen file into the current cwd. Download does
            // the same for the multi-selection (re-using the context-menu
            // batch flow). Refresh re-navigates to the current cwd to force
            // a listing reload.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mb_1()
                    .child(render_sftp_action_button(
                        "sftp-upload-btn",
                        "icons/upload.svg",
                        t!("sftp.upload").to_string(),
                        on_upload.is_some(),
                        {
                            let entity = entity.clone();
                            let on_upload = on_upload.clone();
                            let cwd = cwd.clone();
                            move |_w, cx| {
                                trigger_upload(
                                    entity.clone(),
                                    on_upload.as_ref(),
                                    cwd.as_ref(),
                                    cx,
                                );
                            }
                        },
                    ))
                    .child(render_sftp_action_button(
                        "sftp-download-btn",
                        "icons/download.svg",
                        t!("sftp.download").to_string(),
                        on_download.is_some(),
                        {
                            let entity = entity.clone();
                            let on_download = on_download.clone();
                            let cwd = cwd.clone();
                            move |_w, cx| {
                                trigger_download_from_button(
                                    entity.clone(),
                                    on_download.as_ref(),
                                    cwd.as_ref(),
                                    cx,
                                );
                            }
                        },
                    ))
                    .child(render_sftp_action_button(
                        "sftp-refresh-btn",
                        "icons/refresh-cw.svg",
                        t!("sftp.refresh").to_string(),
                        on_navigate.is_some(),
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
            .child(
                // List + scrollbar. The scrollbar is absolutely positioned
                // (Scrollbar's own layout is `position: absolute`), so we use
                // a relative wrapper and give the list right-padding equal to
                // the scrollbar width. That way the rows' right-side rounded
                // corners land to the left of the scrollbar track instead of
                // being painted over by it.
                //
                // This container is also the drop target for external file
                // drag-in uploads: `on_drop::<ExternalPaths>` fires when the
                // user drops OS files onto the list, and `on_drag_move` tracks
                // whether the cursor is inside the list bounds to drive the
                // `DropZoneOverlay`.
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    // Drop zone for external file drag-in uploads.
                    .when(on_upload.is_some(), |el| {
                        el.on_drop::<ExternalPaths>(move |paths, _w, cx| {
                            let on_upload = on_upload_for_drop.clone();
                            let cwd = cwd_for_drop.clone();
                            let entity = entity_for_drop.clone();
                            if let Some(cwd) = cwd {
                                let cwd_str = cwd.as_str().to_string();
                                let _ = entity.update(cx, |view, cx| {
                                    view.drag_over = false;
                                    cx.notify();
                                });
                                for local in paths.paths() {
                                    let name = local
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| local.to_string_lossy().into_owned());
                                    let remote = if cwd_str.ends_with('/') {
                                        format!("{}{}", cwd_str, name)
                                    } else {
                                        format!("{}/{}", cwd_str, name)
                                    };
                                    if let Some(ref cb) = on_upload {
                                        cb(local.to_string_lossy().into_owned(), remote, cx);
                                    }
                                }
                            }
                        })
                    })
                    // Track external drag-over for the drop-zone overlay.
                    // `on_drag_move` fires for all move events while an
                    // `ExternalPaths` drag is active (even outside this
                    // element), so we can toggle `drag_over` based on
                    // whether the cursor is inside the list bounds.
                    .on_drag_move::<ExternalPaths>({
                        let entity = entity_for_list.clone();
                        move |e, _w, cx| {
                            let _ = entity.update(cx, |view, cx| {
                                let should = e.bounds.contains(&e.event.position);
                                if view.drag_over != should {
                                    view.drag_over = should;
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .child(
                        v_virtual_list(
                            _cx.entity(),
                            "sftp-entries",
                            item_sizes.clone(),
                            move |this, range, _window, cx| {
                                let all_entries = &all_entries;
                                range
                                    .map(|i| {
                                        let (name, is_dir) = &all_entries[i];
                                        let name = name.clone();
                                        let is_dir = *is_dir;
                                        let icon_path = if is_dir {
                                            "icons/folder.svg"
                                        } else {
                                            "icons/file.svg"
                                        };

                                        // Build target path for navigation
                                        let cwd_ref = this.cwd.as_ref().map(|s| s.as_str()).unwrap_or("/");
                                        let target_path = if name == "." {
                                            cwd_ref.to_string()
                                        } else if name == ".." {
                                            let mut parts: Vec<&str> =
                                                cwd_ref.split('/').filter(|s| !s.is_empty()).collect();
                                            parts.pop();
                                            if parts.is_empty() {
                                                "/".to_string()
                                            } else {
                                                format!("/{}", parts.join("/"))
                                            }
                                        } else if cwd_ref.ends_with('/') {
                                            format!("{}{}", cwd_ref, name)
                                        } else {
                                            format!("{}/{}", cwd_ref, name)
                                        };

                                        let on_navigate = this.on_navigate.clone();
                                        let on_download = this.on_download.clone();
                                        let on_delete = this.on_delete.clone();
                                        let on_rename = this.on_rename.clone();
                                        let on_edit = this.on_edit.clone();
                                        let context_menu = this.context_menu.clone();
                                        let alert_controller = this.alert_controller.clone();
                                        let entity = cx.entity().downgrade();
                                        let is_hovered = this.hovered_entry.as_deref() == Some(name.as_str());
                                        let force_highlight =
                                            this.context_menu_entry.as_deref() == Some(name.as_str());
                                        let is_selected = this.selected.contains(name.as_str()) && name != "..";
                                        let is_highlighted = is_hovered || force_highlight;
                                        // Is this row currently being renamed inline?
                                        let is_renaming = renaming_entry.as_deref() == Some(name.as_str());
                                        let row_rename_input = rename_input.clone();
                                        let row_id = ElementId::Name(format!("sftp-{i}").into());
                                        let row_id_for_transition = row_id.clone();
                                        // Whether this row can be dragged (start a
                                        // drag-out-to-download). `.` and `..` are
                                        // navigation helpers, not real entries.
                                        let draggable = name != "." && name != "..";
                                        // Capture the drag value + download cb for
                                        // the on_drag constructor.
                                        let drag_remote_path = target_path.clone();
                                        let drag_name = name.clone();
                                        let drag_is_dir = is_dir;

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
                                            // Allow dragging real entries to
                                            // initiate a drag-out download. The
                                            // drag payload carries the remote
                                            // path + name so a drop target
                                            // (e.g. the terminal area) can
                                            // trigger the download.
                                            .when(draggable, |el| {
                                                el.on_drag(
                                                    SftpDragValue {
                                                        remote_path: drag_remote_path.clone(),
                                                        name: drag_name.clone(),
                                                        is_dir: drag_is_dir,
                                                    },
                                                    |drag_value, _offset, _w, cx| {
                                                        cx.new(|_| drag_value.clone())
                                                    },
                                                )
                                            })
                                            // Left-click drives both navigation (double-click on
                                            // a dir) and multi-selection (cmd/ctrl-click on
                                            // any selectable row). `.` and `..` are excluded
                                            // from selection because they're navigation
                                            // helpers, not real entries.
                                            .on_mouse_down(MouseButton::Left, {
                                                let name = name.clone();
                                                let is_dir = is_dir;
                                                let on_navigate = on_navigate.clone();
                                                let on_edit = on_edit.clone();
                                                let target = target_path.clone();
                                                let entity = entity.clone();
                                                move |event, _w, cx| {
                                                    // Double-click on a directory still
                                                    // navigates regardless of modifiers.
                                                    if is_dir && event.click_count == 2 {
                                                        if let Some(ref cb) = on_navigate {
                                                            cb(target.clone(), cx);
                                                        }
                                                        return;
                                                    }
                                                    // Double-click on a file opens it in
                                                    // the local editor.
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
                                                        // `secondary` is cmd on macOS, ctrl
                                                        // elsewhere — the conventional
                                                        // "add to selection" modifier.
                                                        if event.modifiers.secondary() {
                                                            if view.selected.contains(name.as_str()) {
                                                                view.selected.remove(name.as_str());
                                                            } else {
                                                                view.selected.insert(name.clone());
                                                            }
                                                        } else {
                                                            view.selected.clear();
                                                            view.selected.insert(name.clone());
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
                                                    // Decide which entries this menu acts on.
                                                    // If the right-clicked row is already part
                                                    // of the multi-selection, the menu applies
                                                    // to the whole selection; otherwise the
                                                    // selection snaps to just this row (the
                                                    // standard Finder/Explorer behaviour).
                                                    let pos = event.position;
                                                    let menu_entries = entity
                                                        .update(cx, |view, cx| -> Vec<(String, bool, String)> {
                                                            if !view.selected.contains(name.as_str()) {
                                                                view.selected.clear();
                                                                if name != ".." && name != "." {
                                                                    view.selected.insert(name.clone());
                                                                }
                                                            }
                                                            // Mark this entry as the menu-
                                                            // triggering entry so it keeps the
                                                            // hover background.
                                                            view.context_menu_entry = Some(name.clone());
                                                            cx.notify();
                                                            // Build the list of (name, is_dir,
                                                            // remote_path) the menu will act
                                                            // on. We resolve from the current
                                                            // listing so the paths are fresh.
                                                            let cwd_str = view
                                                                .cwd
                                                                .as_ref()
                                                                .map(|s| s.as_str())
                                                                .unwrap_or("/");
                                                            view.entries
                                                                .iter()
                                                                .filter(|(n, _)| {
                                                                    n != "."
                                                                        && n != ".."
                                                                        && view.selected.contains(n.as_str())
                                                                })
                                                                .map(|(n, d)| {
                                                                    let p = if cwd_str.ends_with('/') {
                                                                        format!("{}{}", cwd_str, n)
                                                                    } else {
                                                                        format!("{}/{}", cwd_str, n)
                                                                    };
                                                                    (n.clone(), *d, p)
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();

                                                    // Build the menu items. The rules:
                                                    //   - A single directory selected → prepend "Enter"
                                                    //     (navigate into it).
                                                    //   - One or more selectable entries → "Download"
                                                    //     (or "Download (N)" for multi-select).
                                                    //   - Right-click on `..` with no selection →
                                                    //     just "Enter" to navigate to parent.
                                                    let mut items: Vec<ContextMenuItem> = Vec::new();

                                                    // "Enter" (navigate) — only when exactly one
                                                    // directory is selected.
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

                                                    // "Open in Editor" — only for a single selected
                                                    // file (directories aren't editable this way).
                                                    // Placed after "Enter" and before "Download" so
                                                    // the order reads: Enter (dirs) → Open in Editor
                                                    // (files) → Download → Rename → Delete.
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
                                                                    view.selected.clear();
                                                                    cx.notify();
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    // "Download" — available whenever there's at
                                                    // least one selectable entry. The backend's
                                                    // `sftp_download` dispatches between file
                                                    // and directory downloads internally (via
                                                    // `stat`), so we don't branch on `is_dir`.
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
                                                        items.push(ContextMenuItem::new(label, move |_w, cx| {
                                                            if to_download.is_empty() {
                                                                return;
                                                            }
                                                            // Clear the multi-selection once the
                                                            // download is dispatched — the user
                                                            // has committed to the batch and
                                                            // lingering highlights would just
                                                            // obscure the next interaction.
                                                            let _ = entity_for_clear.update(cx, |view, cx| {
                                                                view.selected.clear();
                                                                cx.notify();
                                                            });
                                                            trigger_batch_download(
                                                                to_download.clone(),
                                                                on_download.as_ref(),
                                                                cx,
                                                            );
                                                        }));
                                                    }

                                                    // "Rename" — only for a single selected entry
                                                    // (file or dir) that isn't `..`. Sets
                                                    // `renaming_entry` so the row's label is
                                                    // replaced by an inline `StyledInput`
                                                    // on the next render.
                                                    if menu_entries.len() == 1 && name != ".." && on_rename.is_some() {
                                                        let entry_name = menu_entries[0].0.clone();
                                                        let entity_for_rename = entity.clone();
                                                        items.push(ContextMenuItem::new(
                                                            t!("sftp.rename").to_string(),
                                                            move |_w, cx| {
                                                                let _ = entity_for_rename.update(cx, |view, cx| {
                                                                    view.start_rename(entry_name.clone(), _w, cx);
                                                                });
                                                            },
                                                        ));
                                                    }

                                                    // Fallback: right-click on `..` (or `.`)
                                                    // with no selectable entries — offer
                                                    // navigation only.
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
                                                    // Add a "Delete" item for everything
                                                    // except the ".." parent-navigation entry.
                                                    if name != ".." {
                                                        items.push(
                                                            ContextMenuItem::new(t!("sftp.delete").to_string(), {
                                                                let alert_controller = alert_controller.clone();
                                                                let name = name.clone();
                                                                let target_path = target_path.clone();
                                                                let on_delete = on_delete.clone();
                                                                let entity_for_clear = entity.clone();
                                                                move |_w, cx| {
                                                                    let Some(ref ac) = alert_controller else {
                                                                        return;
                                                                    };
                                                                    let target_path = target_path.clone();
                                                                    let on_delete = on_delete.clone();
                                                                    let entity_for_clear = entity_for_clear.clone();
                                                                    ac.update(cx, |c, cx| {
                                                                        c.show(
                                                                            AlertState {
                                                                                severity: AlertSeverity::Danger,
                                                                                title: t!("sftp.delete_title")
                                                                                    .to_string()
                                                                                    .into(),
                                                                                description: Some(
                                                                                    t!(
                                                                                        "sftp.delete_prompt",
                                                                                        name = name.as_str()
                                                                                    )
                                                                                    .to_string()
                                                                                    .into(),
                                                                                ),
                                                                                confirm_label: t!(
                                                                                    "sftp.delete_confirm"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                cancel_label: t!(
                                                                                    "terminal.host_key_cancel"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                on_confirm: Some(Rc::new(
                                                                                    move |_w, cx| {
                                                                                        // Dispatch the actual delete. The
                                                                                        // backend stats the path to decide
                                                                                        // between remove_file / remove_dir.
                                                                                        if let Some(ref cb) =
                                                                                            on_delete
                                                                                        {
                                                                                            cb(
                                                                                                target_path.clone(),
                                                                                                cx,
                                                                                            );
                                                                                        }
                                                                                        // Clear the selection so the
                                                                                        // deleted row's highlight doesn't
                                                                                        // linger — the listing will refresh
                                                                                        // when the backend re-reads the dir.
                                                                                        let _ = entity_for_clear
                                                                                            .update(
                                                                                                cx,
                                                                                                |view, cx| {
                                                                                                    view.selected
                                                                                                        .clear();
                                                                                                    cx.notify();
                                                                                                },
                                                                                            );
                                                                                    },
                                                                                )),
                                                                                ..AlertState::default()
                                                                            },
                                                                            cx,
                                                                        );
                                                                    });
                                                                }
                                                            })
                                                            .danger(true),
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
                                            // Smooth hover color transition. Uses
                                            // `transition_when_else` (not `transition_on_hover`)
                                            // so we can also force the highlight on when a
                                            // context menu triggered by this row is open.
                                            .with_transition(row_id_for_transition)
                                            .on_hover({
                                                let name = name.clone();
                                                move |hovered, _w, cx| {
                                                    let _ = entity.update(cx, |view, cx| {
                                                        if *hovered {
                                                            view.hovered_entry = Some(name.clone());
                                                        } else if view.hovered_entry.as_deref()
                                                            == Some(name.as_str())
                                                        {
                                                            view.hovered_entry = None;
                                                        }
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .transition_when_else(
                                                is_highlighted,
                                                std::time::Duration::from_millis(120),
                                                Linear,
                                                |el| el.bg(rgba((surface_hover() << 8) | 0xFF)),
                                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                                            )
                                            // Selected rows get a persistent blue accent bar
                                            // on the left edge plus a subtle blue tint so the
                                            // selection reads even when not hovered. We render
                                            // this as an absolutely-positioned stripe inside
                                            // the row so it doesn't affect the flex layout.
                                            // The tint is applied only when not highlighted so
                                            // the hover/menu colour takes precedence visually
                                            // when the user is interacting with the row.
                                            .when(is_selected, |el| {
                                                el.relative().child(
                                                    div()
                                                        .absolute()
                                                        .top(px(2.0))
                                                        .bottom(px(2.0))
                                                        .left_0()
                                                        .w(px(2.0))
                                                        .rounded(px(1.0))
                                                        .bg(rgb(btn_primary_bg())),
                                                )
                                            })
                                            .when(is_selected && !is_highlighted, |el| {
                                                el.bg(rgba(input_selection()))
                                            })
                                            .child(
                                                svg()
                                                    .path(icon_path)
                                                    .size(px(14.0))
                                                    .flex_shrink_0()
                                                    .text_color(rgb(text_muted())),
                                            )
                                            // When this row is being renamed, replace the
                                            // static label with an inline `StyledInput`.
                                            // The input was already created + focused in
                                            // `start_rename`; here we just render it in
                                            // place. Otherwise show the entry name as
                                            // usual.
                                            .when_some(
                                                if is_renaming { row_rename_input } else { None },
                                                |el, input| {
                                                    el.child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                StyledInput::new(
                                                                    format!("sftp-rename-{i}"),
                                                                    input,
                                                                )
                                                                .xsmall(),
                                                            ),
                                                    )
                                                },
                                            )
                                            .when(!is_renaming, |el| {
                                                el.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(text_primary()))
                                                        .whitespace_nowrap()
                                                        .overflow_hidden()
                                                        .child(name.clone()),
                                                )
                                            })
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&scroll_handle)
                        .pr(px(10.0)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(10.0))
                            .child(
                                Scrollbar::vertical(&scroll_handle)
                                    .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
                            ),
                    )
                    // Drop-zone overlay: shows a translucent hint with icon
                    // when external files are dragged over the list. Fades
                    // in/out with an eased transition.
                    .child(
                        DropZoneOverlay::new(drag_over)
                            .hint(t!("sftp.drop_upload_hint").to_string())
                            .id("sftp-panel-drop-overlay"),
                    )
                    // Zero-size canvas that registers a window-level
                    // `FileDropEvent` listener during paint. When the OS
                    // reports the drag has left the window entirely
                    // (`Exited`), we clear `drag_over` so the overlay
                    // doesn’t get stuck in the visible state — `on_drag_move`
                    // can’t catch this because `Exited` is not a `MouseMoveEvent`.
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
                                                if view.drag_over {
                                                    view.drag_over = false;
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
            )
    }
}

// ---------------------------------------------------------------------------
// Batch download orchestration
// ---------------------------------------------------------------------------

/// Drive a batch SFTP download.
///
/// `entries` is a list of `(name, is_dir, remote_path)` tuples representing
/// the items the user wants to fetch. A single native folder-picker is shown
/// (so the user isn't prompted once per file); once a destination is chosen,
/// each entry is downloaded into it via the `on_download` callback, which
/// routes to the active terminal's backend (`SshBackend::sftp_download`).
///
/// The backend already dispatches between single-file and directory downloads
/// internally (via `stat`), so we don't need to branch on `is_dir` here — it's
/// only carried along for potential future per-item UI (e.g. a transfer
/// queue).
///
/// Cancellation is silent: if the user dismisses the picker, nothing is
/// downloaded and no error is surfaced.
fn trigger_batch_download(
    entries: Vec<(String, bool, String)>,
    on_download: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    cx: &mut App,
) {
    let Some(on_download) = on_download else {
        return;
    };
    let on_download = on_download.clone();

    // Show a single folder picker for the whole batch. We pick directories
    // only (not files) because we're choosing a destination folder, and we
    // disable multi-select since one destination is enough.
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t!("sftp.download_prompt").to_string().into()),
    });

    cx.spawn(async move |cx| {
        // `oneshot::Receiver` is itself a `Future` — awaiting it yields
        // `Result<T, Canceled>`, where `T` is the platform's
        // `Result<Option<Vec<PathBuf>>>` (outer = platform error, inner =
        // user cancellation as `None`).
        let picked = match rx.await {
            Ok(Ok(Some(mut paths))) => paths.pop(),
            Ok(Ok(None)) => {
                tracing::info!("SFTP download: user cancelled folder picker");
                None
            }
            Ok(Err(e)) => {
                tracing::warn!("SFTP download: folder picker error: {e}");
                None
            }
            Err(e) => {
                tracing::warn!("SFTP download: picker channel closed: {e}");
                None
            }
        };
        let Some(dest_dir) = picked else {
            // User cancelled or picker failed — nothing to do.
            return;
        };
        tracing::info!(
            "SFTP download: dest dir = {}, {} entr{} to fetch",
            dest_dir.display(),
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );

        // Iterate the chosen entries and dispatch each download. We do this
        // inside a single `update` so the callback invocations share one main-
        // thread turn rather than bouncing back to the executor per item.
        let _ = cx.update(|cx| {
            for (name, _is_dir, remote_path) in &entries {
                // Local filename = the entry's basename. We deliberately don't
                // recreate the remote directory hierarchy under `dest_dir` —
                // a flat dump matches what a typical SFTP client does for a
                // multi-selection download.
                let local_path = dest_dir.join(name);
                tracing::info!(
                    "SFTP download: dispatching remote={remote_path} -> local={}",
                    local_path.display()
                );
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

// ---------------------------------------------------------------------------
// Action button row (upload / download / refresh)
// ---------------------------------------------------------------------------

/// Render a compact icon-only action button for the SFTP toolbar. Uses the
/// ghost-button colour scheme (transparent bg, subtle hover) so the row reads
/// as a thin toolbar rather than three prominent buttons.
///
/// `enabled = false` dims the icon and disables the click handler — used when
/// the corresponding backend callback isn't wired (e.g. no active terminal).
fn render_sftp_action_button(
    id: &'static str,
    icon: &'static str,
    tooltip: String,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let color = if enabled { text_muted() } else { 0x45475a };
    let hover_bg = rgba((surface_hover() << 8) | 0xFF);
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded(px(4.0))
        .when(enabled, |el| el.hover(move |s| s.bg(hover_bg)))
        .when(!enabled, |el| el.cursor_not_allowed())
        .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(w, cx))
        .when(enabled, |el| {
            el.on_click(move |_e, w, cx| {
                on_click(w, cx);
                cx.stop_propagation();
            })
        })
        .child(svg().path(icon).size(px(14.0)).text_color(rgb(color)))
}

/// Upload button handler: open a native multi-select file picker and upload
/// each chosen file into the current remote cwd. Mirrors
/// [`trigger_batch_download`] but in the opposite direction.
///
/// Cancellation is silent: dismissing the picker uploads nothing.
fn trigger_upload(
    entity: WeakEntity<SftpPanel>,
    on_upload: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    cwd: Option<&Arc<String>>,
    cx: &mut App,
) {
    let Some(on_upload) = on_upload else {
        return;
    };
    let on_upload = on_upload.clone();
    let Some(cwd) = cwd else {
        return;
    };
    let cwd = cwd.as_str().to_string();

    // Multi-select picker allowing both files and directories. The backend's
    // `sftp_upload` stats each path and dispatches to the file or directory
    // upload flow accordingly — directories get tar+gz'd locally, uploaded,
    // then extracted remotely via `tar xzf`.
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
            Ok(Err(_e)) => {
                #[cfg(debug_assertions)]
                tracing::warn!("SFTP upload: file picker error: {_e}");
                None
            }
            Err(_e) => {
                #[cfg(debug_assertions)]
                tracing::warn!("SFTP upload: picker channel closed: {_e}");
                None
            }
        };
        let Some(paths) = picked else {
            return;
        };
        if paths.is_empty() {
            return;
        }

        // Clear the multi-selection so the upload doesn't look like it's
        // acting on the highlighted rows.
        let _ = entity.update(cx, |view, cx| {
            view.selected.clear();
            cx.notify();
        });

        let _ = cx.update(|cx| {
            for local in &paths {
                let name = local
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| local.to_string_lossy().into_owned());
                let remote = if cwd.ends_with('/') {
                    format!("{}{}", cwd, name)
                } else {
                    format!("{}/{}", cwd, name)
                };
                on_upload(local.to_string_lossy().into_owned(), remote, cx);
            }
        });
    })
    .detach();
}

/// Download button handler: collect the current multi-selection (or, if none,
/// do nothing — the user must select entries first) and run the same batch
/// download flow as the context menu. Re-uses [`trigger_batch_download`] so
/// behaviour stays consistent between the two entry points.
fn trigger_download_from_button(
    entity: WeakEntity<SftpPanel>,
    on_download: Option<&Rc<dyn Fn(String, String, &mut App)>>,
    cwd: Option<&Arc<String>>,
    cx: &mut App,
) {
    // Read the current selection + cwd + entries from the panel so we can
    // resolve remote paths. If nothing is selected, bail silently — the
    // toolbar button is a convenience for acting on an existing selection,
    // not a replacement for the right-click flow.
    let entries = entity.read_with(cx, |view, _cx| {
        let cwd_str = view.cwd.as_ref().map(|s| s.as_str()).unwrap_or("/");
        view.entries
            .iter()
            .filter(|(n, _)| n != "." && n != ".." && view.selected.contains(n.as_str()))
            .map(|(n, d)| {
                let p = if cwd_str.ends_with('/') {
                    format!("{}{}", cwd_str, n)
                } else {
                    format!("{}/{}", cwd_str, n)
                };
                (n.clone(), *d, p)
            })
            .collect::<Vec<_>>()
    });
    let Ok(entries) = entries else {
        return;
    };
    if entries.is_empty() {
        return;
    }
    let _ = cwd; // cwd is read from the entity above to stay fresh
    trigger_batch_download(entries, on_download, cx);
}
