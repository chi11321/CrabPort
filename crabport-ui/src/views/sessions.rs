use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::InteractiveElementExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::components::group_header::group_header;
use crate::motion::{DURATION_FAST, DURATION_MODERATE, EASE_STANDARD, RADIUS_MD};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};

/// Sentinel id used for the virtual "Favorites" group in collapse state.
/// Uses `i64::MAX` so it can never collide with a real group id.
const FAVORITES_GROUP_ID: i64 = i64::MAX;

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

    // -------------------------------------------------------------------
    // Inline group rename (delegates to GroupRenameState)
    // -------------------------------------------------------------------

    fn start_group_rename(
        &mut self,
        group_id: i64,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.group_rename.start(group_id, current_name, window, cx);
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

impl Render for SessionsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.hosts.clone();
        let form_state = self.form_state.clone();
        let app = self.app.clone();
        let on_new = self.on_new.clone();
        let on_connect = self.on_connect.clone();
        let on_sftp_connect = self.on_sftp_connect.clone();
        let on_edit = self.on_edit.clone();
        let on_remove = self.on_remove.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();
        let hovered_host_id = self.hovered_host_id;
        // Load host groups once for the list (group headers + ctxmenu).
        let groups = crate::app_state::AppState::store(_cx)
            .lock()
            .groups(crabport_core::credential::GroupKind::Host)
            .unwrap_or_default();

        // If the global context menu is no longer active, clear the
        // "menu-triggering row" highlight. We do this in render (read-only
        // on the controller) rather than via a callback because the menu's
        // dismiss is async and we have no direct hook into it.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(_cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.context_menu_host_id = None;
        }
        let context_menu_host_id = self.context_menu_host_id;
        let renaming_group_id = self.group_rename.renaming_group_id;
        let rename_input = self.group_rename.rename_input.clone();

        // Partition hosts: ungrouped (group_id is None) first, then one
        // bucket per group.
        let mut ungrouped: Vec<&ConnectionHost> = Vec::new();
        let mut grouped: std::collections::HashMap<i64, Vec<&ConnectionHost>> =
            std::collections::HashMap::new();
        for h in &self.hosts {
            match h.group_id {
                Some(gid) => grouped.entry(gid).or_default().push(h),
                None => ungrouped.push(h),
            }
        }

        // Favorites bucket: every host with `favorite == true`, regardless
        // of which group it belongs to. This is a *virtual* group — the
        // items still appear in their real groups below.
        let favorites: Vec<&ConnectionHost> = self.hosts.iter().filter(|h| h.favorite).collect();

        let collapsed_groups = self.collapsed_groups.clone();
        let favorites_collapsed = self.collapsed_groups.contains(&FAVORITES_GROUP_ID);

        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            // --- Header: title + New button ---
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(text_primary()))
                            .child(t!("sidebar.sessions").to_string()),
                    )
                    .child(
                        Button::new("hosts-new-btn")
                            .primary()
                            .icon("icons/plus.svg")
                            .w_auto()
                            .px_2()
                            .child(t!("sessions.new_button").to_string())
                            .on_click(move |_e, w, cx| {
                                if let Some(ref cb) = on_new {
                                    cb(w, cx);
                                }
                            }),
                    ),
            )
            // --- Separator ---
            .child(div().h_px().bg(rgb(border())).mx_4())
            // --- Hosts list (or empty state) ---
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_4()
                    .py_2()
                    .when_else(
                        hosts.is_empty(),
                        |el| {
                            el.flex().items_center().justify_center().child(
                                div()
                                    .text_color(rgb(text_muted()))
                                    .text_sm()
                                    .child(t!("sessions.empty").to_string()),
                            )
                        },
                        |el| {
                            el.flex()
                                .flex_col()
                                .gap_1()
                                // Ungrouped hosts (favorites float to top).
                                .children(ungrouped.iter().map(|h| {
                                    let host = (*h).clone();
                                    let on_connect = on_connect.clone();
                                    let on_sftp_connect = on_sftp_connect.clone();
                                    let on_edit = on_edit.clone();
                                    let on_remove = on_remove.clone();
                                    let context_menu = context_menu.clone();
                                    let alert_controller = alert_controller.clone();
                                    let is_hovered = hovered_host_id == Some((h.id, false));
                                    let force_highlight = context_menu_host_id == Some((h.id, false));
                                    let entity = _cx.entity().downgrade();

                                    host_row(
                                        &host,
                                        false,
                                        is_hovered,
                                        force_highlight,
                                        entity,
                                        context_menu,
                                        alert_controller,
                                        app.clone(),
                                        groups.clone(),
                                        on_connect.clone(),
                                        on_sftp_connect.clone(),
                                        move |w, cx| {
                                            if let Some(ref cb) = on_edit {
                                                cb(host.id, w, cx);
                                            }
                                        },
                                        move |w, cx| {
                                            if let Some(ref cb) = on_remove {
                                                cb(host.id, w, cx);
                                            }
                                        },
                                    )
                                    .into_any_element()
                                }))
                                // --- Virtual Favorites group (all starred items) ---
                                .when(!favorites.is_empty(), |el| {
                                    let fav_count = favorites.len();
                                    let entity = _cx.entity().downgrade();
                                    let header = group_header(
                                        "host",
                                        FAVORITES_GROUP_ID,
                                        t!("groups.favorites").to_string(),
                                        favorites.len(),
                                        favorites_collapsed,
                                        false,
                                        true,
                                        {
                                            let entity = entity.clone();
                                            Rc::new(move |_w, cx| {
                                                let _ = entity.update(cx, |view, cx| {
                                                    if view
                                                        .collapsed_groups
                                                        .contains(&FAVORITES_GROUP_ID)
                                                    {
                                                        view.collapsed_groups
                                                            .remove(&FAVORITES_GROUP_ID);
                                                    } else {
                                                        view.collapsed_groups
                                                            .insert(FAVORITES_GROUP_ID);
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                        },
                                        None,
                                        None,
                                        false,
                                        None,
                                    )
                                    .into_any_element();

                                    // Build rows for the favorites bucket.
                                    let fav_rows: Vec<AnyElement> = favorites
                                        .iter()
                                        .map(|h| {
                                            let host = (*h).clone();
                                            let on_connect = on_connect.clone();
                                            let on_sftp_connect = on_sftp_connect.clone();
                                            let on_edit = on_edit.clone();
                                            let on_remove = on_remove.clone();
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let is_hovered = hovered_host_id == Some((h.id, true));
                                            let force_highlight =
                                                context_menu_host_id == Some((h.id, true));
                                            let entity = _cx.entity().downgrade();

                                            host_row(
                                                &host,
                                                true,
                                                is_hovered,
                                                force_highlight,
                                                entity,
                                                context_menu,
                                                alert_controller,
                                                app.clone(),
                                                groups.clone(),
                                                on_connect.clone(),
                                                on_sftp_connect.clone(),
                                                move |w, cx| {
                                                    if let Some(ref cb) = on_edit {
                                                        cb(host.id, w, cx);
                                                    }
                                                },
                                                move |w, cx| {
                                                    if let Some(ref cb) = on_remove {
                                                        cb(host.id, w, cx);
                                                    }
                                                },
                                            )
                                            .into_any_element()
                                        })
                                        .collect();

                                    let body_id = ElementId::Name(
                                        format!("host-group-body-{}", FAVORITES_GROUP_ID).into(),
                                    );
                                    let body = div()
                                        .id(body_id.clone())
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_hidden()
                                        .with_transition(body_id)
                                        .transition_when_else(
                                            !favorites_collapsed,
                                            DURATION_MODERATE,
                                            EASE_STANDARD,
                                            move |el| {
                                                el.h(px(fav_count as f32 * 62.0 - 4.0)).opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(fav_rows)
                                        .into_any_element();

                                    el.child(header).child(body)
                                })
                                // One section per group: header immediately
                                // followed by its rows (wrapped in an
                                // animated container so collapse/expand
                                // eases height + opacity).
                                .children(groups.iter().flat_map(|g| {
                                    let gid = g.id;
                                    let group = g.clone();
                                    let members = grouped.get(&gid).cloned().unwrap_or_default();
                                    let member_count = members.len();
                                    let is_collapsed = collapsed_groups.contains(&gid);
                                    let entity = _cx.entity().downgrade();

                                    // Header first.
                                    let header = group_header(
                                        "host",
                                        gid,
                                        group.name.clone(),
                                        members.len(),
                                        is_collapsed,
                                        group.favorite,
                                        false,
                                        {
                                            let entity = entity.clone();
                                            Rc::new(move |_w, cx| {
                                                let _ = entity.update(cx, |view, cx| {
                                                    if view.collapsed_groups.contains(&gid) {
                                                        view.collapsed_groups.remove(&gid);
                                                    } else {
                                                        view.collapsed_groups.insert(gid);
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                        },
                                        None,
                                        Some({
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let app = app.clone();
                                            let group_for_menu = group.clone();
                                            Rc::new(move |event, _w, cx| {
                                                let Some(ref cm) = context_menu else {
                                                    return;
                                                };
                                                let pos = event.position;
                                                let alert_controller = alert_controller.clone();
                                                let app = app.clone();
                                                let group = group_for_menu.clone();
                                                cm.update(cx, |c, cx| {
                                                    let mut items: Vec<ContextMenuItem> = Vec::new();

                                                    // Rename Group
                                                    items.push(ContextMenuItem::new(
                                                        t!("hosts.rename_group").to_string(),
                                                        {
                                                            let group = group.clone();
                                                            let entity = entity.clone();
                                                            move |w, cx| {
                                                                let _ = entity.update(cx, |view, cx| {
                                                                    view.start_group_rename(group.id, group.name.clone(), w, cx);
                                                                });
                                                            }
                                                        },
                                                    ));

                                                    // Delete Group (with confirmation)
                                                    items.push(
                                                        ContextMenuItem::new(
                                                            t!("hosts.delete_group").to_string(),
                                                            {
                                                                let alert_controller =
                                                                    alert_controller.clone();
                                                                let app = app.clone();
                                                                let group = group.clone();
                                                                move |_w, cx| {
                                                                    let Some(ref ac) =
                                                                        alert_controller
                                                                    else {
                                                                        return;
                                                                    };
                                                                    let app = app.clone();
                                                                    let gid = group.id;
                                                                    let name =
                                                                        group.name.clone();
                                                                    ac.update(cx, |c, cx| {
                                                                        c.show(
                                                                            AlertState {
                                                                                severity:
                                                                                    AlertSeverity::Danger,
                                                                                title: t!(
                                                                                    "hosts.delete_group"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                description: Some(
                                                                                    t!(
                                                                                        "hosts.delete_group_prompt",
                                                                                        name = name.as_str()
                                                                                    )
                                                                                    .to_string()
                                                                                    .into(),
                                                                                ),
                                                                                confirm_label: t!(
                                                                                    "hosts.delete"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                cancel_label: t!(
                                                                                    "terminal.host_key_cancel"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                on_confirm: Some(
                                                                                    Rc::new(move |_w, cx| {
                                                                                        app.update(cx, |app, cx| {
                                                                                            app.remove_group(gid, crabport_core::credential::GroupKind::Host, cx);
                                                                                        });
                                                                                    }),
                                                                                ),
                                                                                ..AlertState::default()
                                                                            },
                                                                            cx,
                                                                        );
                                                                    });
                                                                }
                                                            },
                                                        )
                                                        .danger(true),
                                                    );

                                                    c.show(
                                                        ContextMenuState {
                                                            position: pos,
                                                            items,
                                                            ..ContextMenuState::default()
                                                        },
                                                        cx,
                                                    );
                                                });
                                            })
                                        }),
                                        renaming_group_id == Some(gid),
                                        if renaming_group_id == Some(gid) {
                                            rename_input.clone()
                                        } else {
                                            None
                                        },
                                    )
                                    .into_any_element();

                                    // Build rows always (even when collapsed) so
                                    // the expand animation has content to reveal.
                                    let rows: Vec<AnyElement> = members
                                        .iter()
                                        .map(|h| {
                                            let host = (*h).clone();
                                            let on_connect = on_connect.clone();
                                            let on_sftp_connect = on_sftp_connect.clone();
                                            let on_edit = on_edit.clone();
                                            let on_remove = on_remove.clone();
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let is_hovered = hovered_host_id == Some((h.id, false));
                                            let force_highlight =
                                                context_menu_host_id == Some((h.id, false));
                                            let entity = _cx.entity().downgrade();

                                            host_row(
                                                &host,
                                                false,
                                                is_hovered,
                                                force_highlight,
                                                entity,
                                                context_menu,
                                                alert_controller,
                                                app.clone(),
                                                groups.clone(),
                                                on_connect.clone(),
                                                on_sftp_connect.clone(),
                                                move |w, cx| {
                                                    if let Some(ref cb) = on_edit {
                                                        cb(host.id, w, cx);
                                                    }
                                                },
                                                move |w, cx| {
                                                    if let Some(ref cb) = on_remove {
                                                        cb(host.id, w, cx);
                                                    }
                                                },
                                            )
                                            .into_any_element()
                                        })
                                        .collect();

                                    // Animated body: collapses to h_0 +
                                    // opacity_0 when collapsed.
                                    let body_id =
                                        ElementId::Name(format!("host-group-body-{}", gid).into());
                                    let body = div()
                                        .id(body_id.clone())
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_hidden()
                                        .with_transition(body_id)
                                        .transition_when_else(
                                            !is_collapsed,
                                            DURATION_MODERATE,
                                            EASE_STANDARD,
                                            move |el| {
                                                el.h(px(member_count as f32 * 62.0 - 4.0))
                                                    .opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(rows)
                                        .into_any_element();

                                    std::iter::once(header)
                                        .chain(std::iter::once(body))
                                        .collect::<Vec<_>>()
                                }))
                        },
                    ),
            )
            // --- Connection form overlay ---
            .when_some(form_state, |el, state| {
                el.child(ConnectionFormView::new(&state, app))
            })
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
    groups: Vec<crabport_core::credential::GroupEntry>,
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
    let is_highlighted = is_hovered || force_highlight;

    let on_connect_for_dblclick = on_connect.clone();
    let host_id_for_dblclick = host_id;

    div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(RADIUS_MD)
        .bg(rgb(bg_base()))
        .on_double_click(move |_, w, cx| {
            gpui_animation::reset_transition(&row_id_clone);
            if let Some(ref cb) = on_connect_for_dblclick {
                cb(host_id_for_dblclick, w, cx);
            }
        })
        // Right-click context menu: Connect, Favorite, Move-to-Group,
        // Edit, Delete. Also record which row triggered the menu so it
        // stays highlighted while the menu is open.
        .on_mouse_down(MouseButton::Right, {
            let on_edit = Rc::new(on_edit);
            let on_remove = Rc::new(on_remove);
            let entity = entity.clone();
            let on_connect = on_connect.clone();
            let on_sftp_connect = on_sftp_connect.clone();
            let app = app.clone();
            let groups = groups.clone();
            let host_kind = host.kind;
            move |event, _w, cx| {
                let Some(ref cm) = context_menu else {
                    return;
                };
                // Mark this row as the menu-triggering row so it keeps the
                // hover background while the menu is up.
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
                let _groups_for_menu = groups.clone();
                let host_favorite_for_menu = host_favorite;
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
                    let favorite_label = if host_favorite_for_menu {
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
                    items.push(
                        ContextMenuItem::new(t!("hosts.delete").to_string(), {
                            let on_remove = on_remove.clone();
                            let alert_controller = alert_controller.clone();
                            let host_name = host_name.clone();
                            move |_w, cx| {
                                let Some(ref ac) = alert_controller else {
                                    return;
                                };
                                let on_remove = on_remove.clone();
                                ac.update(cx, |c, cx| {
                                    c.show(
                                        AlertState {
                                            severity: AlertSeverity::Danger,
                                            title: t!("hosts.delete_title").to_string().into(),
                                            description: Some(
                                                t!(
                                                    "hosts.delete_prompt",
                                                    name = host_name.as_str()
                                                )
                                                .to_string()
                                                .into(),
                                            ),
                                            confirm_label: t!("hosts.delete_confirm")
                                                .to_string()
                                                .into(),
                                            cancel_label: t!("terminal.host_key_cancel")
                                                .to_string()
                                                .into(),
                                            on_confirm: Some(Rc::new(move |w, cx| {
                                                on_remove(w, cx);
                                            })),
                                            ..AlertState::default()
                                        },
                                        cx,
                                    );
                                });
                            }
                        })
                        .danger(true),
                    );

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
        // Track hover of the whole row so the background color eases in.
        // State lives in the SessionsView entity itself.
        .with_transition(row_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                if *hovered {
                    view.hovered_host_id = Some((host_id, is_favorite_copy));
                } else {
                    if view.hovered_host_id == Some((host_id, is_favorite_copy)) {
                        view.hovered_host_id = None;
                    }
                }
                cx.notify();
            });
        })
        .transition_when_else(
            is_highlighted,
            DURATION_FAST,
            EASE_STANDARD,
            |el| el.bg(rgb(surface_active())),
            |el| el.bg(rgb(bg_base())),
        )
        // Host info (name + address)
        .child(
            div()
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
                ),
        )
        // Favorite star toggle (far right). Fades in on hover; stays visible
        // (yellow) when already favorited so the user can see + unstar.
        .child({
            let app = app.clone();
            let star_id = ElementId::Name(format!("host-star-{}", host.id).into());
            let star_visible = is_highlighted || host_favorite;
            div()
                .id(star_id.clone())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/star.svg")
                        .size_4()
                        .text_color(rgb(if host_favorite {
                            term_yellow()
                        } else {
                            text_muted()
                        })),
                )
                .with_transition(star_id)
                .transition_when_else(
                    star_visible,
                    DURATION_FAST,
                    EASE_STANDARD,
                    |el| el.opacity(1.0),
                    |el| el.opacity(0.0),
                )
                .on_click(move |_e, _w, cx| {
                    app.update(cx, |app, cx| {
                        app.toggle_host_favorite(host_id, cx);
                    });
                })
        })
}
