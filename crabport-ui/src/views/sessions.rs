use std::collections::HashSet;
use std::rc::Rc;

use gpui::*;
use gpui_component::InteractiveElementExt;
use rust_i18n::t;

use crabport_core::credential::GroupKind;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::context_menu::{
    ContextMenuController, ContextMenuItem, ContextMenuState, confirm_delete_item,
};
use crate::components::dialog::AlertController;
use crate::components::list_row::{favorite_star, interactive_row};
use crate::views::collection::{GroupedListView, render_grouped_list};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};

// ---------------------------------------------------------------------------
// Submodules & re-exports
// ---------------------------------------------------------------------------
//
// The connection form (state + view + render helpers) lives in `form.rs`,
// mirroring `views/tunnels/form.rs`. `with_proxy` and `with_certificate` are
// the proxy / certificate sub-form components used by the SSH pane.

pub mod form;
pub mod with_certificate;
pub mod with_proxy;

pub use form::{AuthKind, ConnectionFormState, ConnectionFormView, ConnectionKind};

/// A saved connection host entry.
#[derive(Clone)]
pub struct ConnectionHost {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub kind: crate::views::sessions::ConnectionKind,
    pub credential_id: Option<i64>,
    pub last_login: Option<i64>,
    pub favorite: bool,
    /// FK into the `proxies` table. `None` means no proxy.
    pub proxy_id: Option<i64>,
    /// FK into the `groups` table. `None` means ungrouped.
    pub group_id: Option<i64>,
}

impl From<crabport_core::credential::HostEntry> for ConnectionHost {
    fn from(h: crabport_core::credential::HostEntry) -> Self {
        ConnectionHost {
            id: h.id,
            name: h.name,
            host: h.host,
            port: h.port,
            username: h.username,
            kind: h.kind.into(),
            credential_id: h.credential_id,
            last_login: h.last_login,
            favorite: h.favorite,
            proxy_id: h.proxy_id,
            group_id: h.group_id,
        }
    }
}

/// Hosts sidebar view.
///
/// Holds its own hover state (`hovered_host_id`) so the action buttons can
/// fade in with easing when the row is hovered — without polluting
/// `CrabportApp` state or risking "already being updated" panics.
pub struct SessionsView {
    /// The host row currently being hovered, if any. Keyed by
    /// `(id, is_favorite_copy)` so the favorites copy of an item and its
    /// real-group copy don't share hover state (they'd otherwise
    /// cross-highlight because both match the same id).
    hovered_host_id: Option<(i64, bool)>,
    /// The host row that triggered the currently-open context menu, if any.
    /// While set, that row stays highlighted in the hover color even though
    /// the mouse has moved to the overlay.
    context_menu_host_id: Option<(i64, bool)>,
    // External data pushed in before each render.
    hosts: Vec<ConnectionHost>,
    form_state: Option<ConnectionFormState>,
    app: Entity<CrabportApp>,
    // Global context menu host, used for the right-click menu on each row.
    context_menu: Option<Entity<ContextMenuController>>,
    // Global alert dialog host, used for the delete-confirmation prompt.
    alert_controller: Option<Entity<AlertController>>,
    // Callbacks
    on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    on_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    /// Connect to a host in SFTP-only mode (right-click → "Connect via SFTP").
    /// Only called for SSH hosts.
    on_sftp_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    /// Per-group collapse state for the grouped list.
    collapsed_groups: HashSet<i64>,
    /// Shared inline group-rename state (id + InputState).
    group_rename: GroupRenameState,
}

impl SessionsView {
    pub fn new(app: Entity<CrabportApp>) -> Self {
        Self {
            hovered_host_id: None,
            context_menu_host_id: None,
            hosts: Vec::new(),
            form_state: None,
            app,
            context_menu: None,
            alert_controller: None,
            on_new: None,
            on_connect: None,
            on_sftp_connect: None,
            on_edit: None,
            on_remove: None,
            collapsed_groups: HashSet::new(),
            group_rename: GroupRenameState::new(),
        }
    }

    /// Push the latest external state into the view before render.
    #[allow(clippy::too_many_arguments)]
    pub fn set_state(
        &mut self,
        hosts: Vec<ConnectionHost>,
        form_state: Option<ConnectionFormState>,
        on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        on_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_sftp_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        cx: &mut Context<Self>,
    ) {
        // Clear stale hover if the host disappeared.
        if let Some((id, _)) = self.hovered_host_id
            && !hosts.iter().any(|h| h.id == id)
        {
            self.hovered_host_id = None;
        }
        self.hosts = hosts;
        self.form_state = form_state;
        self.on_new = on_new;
        self.on_connect = on_connect;
        self.on_sftp_connect = on_sftp_connect;
        self.on_edit = on_edit;
        self.on_remove = on_remove;
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        // Note: do NOT call cx.notify() here — set_state is invoked every
        // render from render_content, so notifying would cause an infinite
        // loop. The SessionsView re-renders naturally because its parent
        // (CrabportApp) re-renders.
        let _ = cx;
    }
}

impl GroupRenameView for SessionsView {
    fn group_rename(&mut self) -> &mut GroupRenameState {
        &mut self.group_rename
    }

    fn app_entity(&self) -> &Entity<CrabportApp> {
        &self.app
    }
}

impl GroupedListView for SessionsView {
    type Item = ConnectionHost;

    const ID_PREFIX: &'static str = "host";
    const MENU_NS: &'static str = "hosts";
    const GROUP_KIND: GroupKind = GroupKind::Host;
    const ROW_HEIGHT: f32 = 62.0;
    const SKIP_EMPTY_GROUPS: bool = false;
    const WRAP_SECTIONS: bool = false;
    const TITLE_KEY: &'static str = "sidebar.sessions";
    const NEW_BUTTON_ID: &'static str = "hosts-new-btn";
    const NEW_BUTTON_KEY: &'static str = "sessions.new_button";
    const EMPTY_KEY: &'static str = "sessions.empty";

    fn items(&self) -> &[ConnectionHost] {
        &self.hosts
    }
    fn item_id(item: &ConnectionHost) -> i64 {
        item.id
    }
    fn item_group_id(item: &ConnectionHost) -> Option<i64> {
        item.group_id
    }
    fn item_favorite(item: &ConnectionHost) -> bool {
        item.favorite
    }

    fn context_menu_handle(&self) -> Option<Entity<ContextMenuController>> {
        self.context_menu.clone()
    }
    fn alert_handle(&self) -> Option<Entity<AlertController>> {
        self.alert_controller.clone()
    }
    fn on_new_cb(&self) -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
        self.on_new.clone()
    }

    fn hover_state(&self) -> Option<(i64, bool)> {
        self.hovered_host_id
    }
    fn menu_row_state(&mut self) -> &mut Option<(i64, bool)> {
        &mut self.context_menu_host_id
    }
    fn collapsed_groups(&mut self) -> &mut HashSet<i64> {
        &mut self.collapsed_groups
    }

    fn render_row(
        &self,
        host: &ConnectionHost,
        is_favorite_copy: bool,
        is_hovered: bool,
        force_highlight: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host_id = host.id;
        let on_edit = self.on_edit.clone();
        let on_remove = self.on_remove.clone();
        host_row(
            host,
            is_favorite_copy,
            is_hovered,
            force_highlight,
            cx.entity().downgrade(),
            self.context_menu.clone(),
            self.alert_controller.clone(),
            self.app.clone(),
            self.on_connect.clone(),
            self.on_sftp_connect.clone(),
            move |w, cx| {
                if let Some(ref cb) = on_edit {
                    cb(host_id, w, cx);
                }
            },
            move |w, cx| {
                if let Some(ref cb) = on_remove {
                    cb(host_id, w, cx);
                }
            },
        )
        .into_any_element()
    }

    fn render_overlay(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        self.form_state
            .as_ref()
            .map(|state| ConnectionFormView::new(state, self.app.clone()).into_any_element())
    }
}

impl Render for SessionsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_grouped_list(self, cx)
    }
}

// ---------------------------------------------------------------------------
// Host row
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn host_row(
    host: &ConnectionHost,
    is_favorite_copy: bool,
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<SessionsView>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert_controller: Option<Entity<AlertController>>,
    app: Entity<CrabportApp>,
    on_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_sftp_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    on_remove: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The favorites bucket renders the same host a second time (the item
    // also appears under its real group below). Each instance needs a
    // distinct transition id, otherwise hover state is shared across both
    // and they animate in lockstep. Suffix "-fav" disambiguates the copy.
    let row_id = ElementId::Name(
        format!(
            "host-row-{}{}",
            host.id,
            if is_favorite_copy { "-fav" } else { "" }
        )
        .into(),
    );
    let row_id_clone = row_id.clone();

    let host_id = host.id;
    let host_name = host.name.clone();
    let host_favorite = host.favorite;
    let host_kind = host.kind;
    let is_highlighted = is_hovered || force_highlight;

    let on_connect_for_dblclick = on_connect.clone();

    // Host info (name + address)
    let info = div()
        .flex()
        .flex_col()
        .min_w_0()
        .flex_1()
        .child(
            div()
                .text_sm()
                .text_color(rgb(text_primary()))
                .child(host.name.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(text_muted()))
                .child(format!("{}@{}:{}", host.username, host.host, host.port)),
        )
        .into_any_element();

    let star = favorite_star(
        "host",
        host_id,
        host_favorite,
        is_highlighted || host_favorite,
        {
            let app = app.clone();
            move |cx| {
                app.update(cx, |app, cx| {
                    app.toggle_host_favorite(host_id, cx);
                });
            }
        },
    )
    .into_any_element();

    interactive_row(
        row_id,
        is_highlighted,
        entity.clone(),
        (host_id, is_favorite_copy),
        |view| &mut view.hovered_host_id,
        move |el| {
            el.on_double_click(move |_, w, cx| {
                gpui_animation::reset_transition(&row_id_clone);
                if let Some(ref cb) = on_connect_for_dblclick {
                    cb(host_id, w, cx);
                }
            })
            // Right-click context menu: Connect, Connect via SFTP (SSH
            // only), Favorite, Edit, Delete. Also record which row
            // triggered the menu so it stays highlighted while it's open.
            .on_mouse_down(MouseButton::Right, {
                let on_edit = Rc::new(on_edit);
                let on_remove = Rc::new(on_remove);
                move |event, _w, cx| {
                    let Some(ref cm) = context_menu else {
                        return;
                    };
                    // Mark this row as the menu-triggering row so it keeps
                    // the hover background while the menu is up.
                    let _ = entity.update(cx, |view, cx| {
                        view.context_menu_host_id = Some((host_id, is_favorite_copy));
                        cx.notify();
                    });
                    let pos = event.position;
                    let on_edit = on_edit.clone();
                    let on_remove = on_remove.clone();
                    let on_connect = on_connect.clone();
                    let on_sftp_connect = on_sftp_connect.clone();
                    let app_for_menu = app.clone();
                    let alert_controller = alert_controller.clone();
                    let host_name = host_name.clone();
                    cm.update(cx, |c, cx| {
                        let mut items: Vec<ContextMenuItem> = Vec::new();

                        // Connect
                        items.push(
                            ContextMenuItem::new(t!("hosts.connect").to_string(), {
                                let on_connect = on_connect.clone();
                                move |w, cx| {
                                    if let Some(ref cb) = on_connect {
                                        cb(host_id, w, cx);
                                    }
                                }
                            })
                            .divider_after(),
                        );

                        // Connect via SFTP (SSH hosts only)
                        if host_kind == crate::views::sessions::ConnectionKind::SSH {
                            items.push(
                                ContextMenuItem::new(t!("hosts.connect_sftp").to_string(), {
                                    let on_sftp_connect = on_sftp_connect.clone();
                                    move |w, cx| {
                                        if let Some(ref cb) = on_sftp_connect {
                                            cb(host_id, w, cx);
                                        }
                                    }
                                })
                                .divider_after(),
                            );
                        }

                        // Favorite toggle
                        let favorite_label = if host_favorite {
                            t!("hosts.unfavorite").to_string()
                        } else {
                            t!("hosts.favorite").to_string()
                        };
                        items.push(
                            ContextMenuItem::new(favorite_label, {
                                let app = app_for_menu.clone();
                                move |_w, cx| {
                                    app.update(cx, |app, cx| {
                                        app.toggle_host_favorite(host_id, cx);
                                    });
                                }
                            })
                            .divider_after(),
                        );

                        // Edit
                        items.push(ContextMenuItem::new(t!("hosts.edit").to_string(), {
                            let on_edit = on_edit.clone();
                            move |w, cx| {
                                on_edit(w, cx);
                            }
                        }));

                        // Delete (with confirmation)
                        items.push(confirm_delete_item(
                            t!("hosts.delete").to_string(),
                            alert_controller.clone(),
                            {
                                let host_name = host_name.clone();
                                move || {
                                    (
                                        t!("hosts.delete_title").to_string().into(),
                                        Some(
                                            t!("hosts.delete_prompt", name = host_name.as_str())
                                                .to_string()
                                                .into(),
                                        ),
                                        t!("hosts.delete_confirm").to_string().into(),
                                    )
                                }
                            },
                            {
                                let on_remove = on_remove.clone();
                                Rc::new(move |w, cx| {
                                    on_remove(w, cx);
                                })
                            },
                        ));

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
        },
        vec![info, star],
    )
}
