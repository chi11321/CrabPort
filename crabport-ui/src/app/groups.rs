//! Group form dialog methods + host-domain favorite/group CRUD for
//! `CrabportApp`.
//!
//! `GroupFormState` (the shared new/rename-group overlay) is owned by
//! `CrabportApp::group_form`. The open/save/close methods here lazily create
//! the form state on first use (mirroring `open_snippet_form_for_create`).
//!
//! The host-specific favorite toggle + group assignment live here too because
//! they reload `self.hosts` from the store so the list re-sorts (favorites
//! float to the top) and re-groups immediately.

use gpui::*;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind};

use super::CrabportApp;
use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::groups::{GroupFormOutput, GroupFormState};
use crate::views::sessions::ConnectionHost;

impl CrabportApp {
    // -----------------------------------------------------------------------
    // Group form (shared across Host / Snippet / Tunnel)
    // -----------------------------------------------------------------------

    /// Open the shared group form dialog in create mode for the given
    /// collection kind. Lazily creates the `GroupFormState` entity on first
    /// use (so the `InputState` is bound to the right window).
    pub fn open_group_form_for_create(&mut self, kind: GroupKind, cx: &mut Context<Self>) {
        self.ensure_group_form(cx);
        let window_handle = cx.active_window();
        if let Some(ref mut form) = self.group_form {
            if let Some(handle) = window_handle {
                let _ = handle.update(cx, |_, window, cx| {
                    form.open_for_create(kind, window, cx);
                });
            } else {
                form.kind = kind;
                form.editing_id = None;
                form.open = true;
                form.name_error = None;
            }
        }
        cx.notify();
    }

    /// Open the shared group form dialog in rename mode, populated from an
    /// existing group.
    pub fn open_group_form_for_edit(&mut self, group: &GroupEntry, cx: &mut Context<Self>) {
        self.ensure_group_form(cx);
        let window_handle = cx.active_window();
        if let Some(ref mut form) = self.group_form {
            if let Some(handle) = window_handle {
                let group = group.clone();
                let _ = handle.update(cx, |_, window, cx| {
                    form.open_for_edit(&group, window, cx);
                });
            } else {
                form.editing_id = Some(group.id);
                form.kind = group.kind;
                form.open = true;
                form.name_error = None;
            }
        }
        cx.notify();
    }

    /// Close the group form dialog. The state is dropped after the exit
    /// animation so the next open can rebind the `InputState` to the active
    /// window.
    pub fn close_group_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.group_form {
            form.close();
        }
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.group_form.is_some() {
                    app.group_form = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Persist a group (insert or rename) from the form output. Dispatches to
    /// the store's `add_group` / `update_group` based on `editing_id`.
    pub fn save_group_form(
        &mut self,
        out: GroupFormOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = AppState::store(cx);
        let name = out.name.trim().to_string();
        let kind = out.kind;
        let result = match out.editing_id {
            Some(id) => store.lock().update_group(id, &name, None),
            None => store.lock().add_group(&name, kind, None).map(|_| ()),
        };
        match result {
            Ok(()) => {
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("groups.save").to_string())
                            .level(NotificationLevel::Success)
                            .duration(std::time::Duration::from_secs(2)),
                        cx,
                    );
                });
            }
            Err(e) => {
                tracing::error!("group save failed: {e}");
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("groups.save").to_string())
                            .level(NotificationLevel::Danger)
                            .message(e.to_string())
                            .duration(std::time::Duration::from_secs(5)),
                        cx,
                    );
                });
            }
        }
        self.close_group_form(cx);
    }

    /// Lazily create the `GroupFormState` entity + wire its callbacks. No-op
    /// if the form already exists. Returns early when there is no active
    /// window (rare, e.g. during shutdown) — the next open attempt with a
    /// live window will succeed.
    fn ensure_group_form(&mut self, cx: &mut Context<Self>) {
        if self.group_form.is_some() {
            return;
        }
        let Some(handle) = cx.active_window() else {
            return;
        };
        let mut form_opt: Option<GroupFormState> = None;
        let _ = handle.update(cx, |_, window, cx| {
            form_opt = Some(GroupFormState::new(window, cx));
        });
        let Some(mut form) = form_opt else {
            return;
        };
        let app = cx.entity().clone();
        form.on_close = Some(std::rc::Rc::new(move |_w, cx| {
            app.update(cx, |app, cx| app.close_group_form(cx));
        }));
        let app = cx.entity().clone();
        form.on_save = Some(std::rc::Rc::new(move |out, w, cx| {
            app.update(cx, |app, cx| app.save_group_form(out, w, cx));
        }));
        self.group_form = Some(form);
    }

    // -----------------------------------------------------------------------
    // Group favorite toggle + delete
    // -----------------------------------------------------------------------

    /// Toggle the favorite flag for a group. Favorite groups sort above
    /// non-favorite groups within the same kind.
    pub fn toggle_group_favorite(&mut self, group_id: i64, cx: &mut Context<Self>) {
        let _ = AppState::store(cx).lock().toggle_group_favorite(group_id);
        cx.notify();
    }

    /// Rename a group in place (used by the inline rename editor on group
    /// headers). Persists via `update_group` (name only, sort_order
    /// unchanged) and notifies so the header re-renders with the new name.
    pub fn rename_group(&mut self, group_id: i64, name: &str, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Err(e) = AppState::store(cx)
            .lock()
            .update_group(group_id, &name, None)
        {
            tracing::error!("rename_group failed: {e}");
            return;
        }
        cx.notify();
    }

    /// Delete a group by id. The store uses `ON DELETE SET NULL` on the
    /// `group_id` FK in `hosts` / `snippets` / `tunnels`, so every member
    /// falls back to "ungrouped" automatically. We then refresh the
    /// in-memory collections so the list re-renders under "Ungrouped"
    /// immediately (the SQL side is already correct, but the cached
    /// `self.hosts` Vec and the tunnel registry still hold the stale
    /// `group_id`).
    ///
    /// `kind` selects which in-memory cache to refresh after the delete.
    pub fn remove_group(&mut self, group_id: i64, kind: GroupKind, cx: &mut Context<Self>) {
        if let Err(e) = AppState::store(cx).lock().remove_group(group_id) {
            tracing::error!("remove_group failed: {e}");
            return;
        }
        match kind {
            GroupKind::Host => {
                self.reload_hosts(cx);
            }
            GroupKind::Snippet => {
                // Snippets are re-read from the store on every render via
                // `set_state`, so no in-memory cache to refresh — a notify
                // is enough.
            }
            GroupKind::Tunnel => {
                // Mirror the SQL `SET NULL` into the registry so the running
                // tunnels drop their stale `group_id` without a full reload.
                let tunnels = self.app_ctx.tunnels.clone();
                let views = tunnels.list();
                for v in views {
                    if v.group_id == Some(group_id) {
                        tunnels.set_group(v.id, None);
                    }
                }
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Host favorite + group assignment
    // -----------------------------------------------------------------------

    /// Toggle the favorite flag for a saved host and reload `self.hosts` so
    /// the list re-sorts (favorites float to the top within the ungrouped
    /// section).
    pub fn toggle_host_favorite(&mut self, host_id: i64, cx: &mut Context<Self>) {
        let _ = AppState::store(cx).lock().toggle_host_favorite(host_id);
        self.reload_hosts(cx);
        cx.notify();
    }

    /// Move a host to a different group (`None` = ungrouped) and reload
    /// `self.hosts` so the list re-renders under the new group header.
    pub fn set_host_group(&mut self, host_id: i64, group_id: Option<i64>, cx: &mut Context<Self>) {
        if let Err(e) = AppState::store(cx).lock().set_host_group(host_id, group_id) {
            tracing::error!("set_host_group failed: {e}");
        }
        self.reload_hosts(cx);
        cx.notify();
    }

    /// Reload `self.hosts` from the store, mapping core `HostEntry` →
    /// `ConnectionHost`. Mirrors the reload loop in `connect_to_host`.
    fn reload_hosts(&mut self, cx: &mut Context<Self>) {
        use crabport_core::credential::HostKind as CoreHostKind;
        if let Ok(all) = AppState::store(cx).lock().hosts() {
            self.hosts = all
                .into_iter()
                .map(|h| ConnectionHost {
                    id: h.id,
                    name: h.name,
                    host: h.host,
                    port: h.port,
                    username: h.username,
                    kind: match h.kind {
                        CoreHostKind::Ssh => crate::views::sessions::ConnectionKind::SSH,
                        CoreHostKind::Telnet => crate::views::sessions::ConnectionKind::Telnet,
                        CoreHostKind::Serial => crate::views::sessions::ConnectionKind::Serial,
                    },
                    credential_id: h.credential_id,
                    last_login: h.last_login,
                    favorite: h.favorite,
                    proxy_id: h.proxy_id,
                    group_id: h.group_id,
                })
                .collect();
        }
    }
}
