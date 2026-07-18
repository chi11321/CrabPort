//! Tunnels management view — the full-page sidebar view for managing saved
//! SSH port-forwarding tunnels (Local / Remote / Dynamic).
//!
//! Mirrors [`crate::views::sessions::SessionsView`] in structure: a header with a
//! "New" button, a scrollable list of rows with hover-fade action buttons,
//! and a right-click context menu (Start/Stop, Edit, Delete) plus an alert
//! confirmation dialog for delete.
//!
//! The view is stateless beyond hover + the "menu-triggering row" highlight.
//! External state (tunnel list, host list, callbacks, global controllers) is
//! pushed in via [`TunnelsView::set_state`] immediately before each render by
//! the parent (`render_content`).

/// Tunnel create/edit form dialog (lives in `form.rs`).
pub mod form;
/// Runtime state + registry for tunnels (lives in `state.rs`).
pub mod state;

// Re-export the commonly-used types so callers can reach them via
// `crate::views::tunnels::TunnelRegistry` / `TunnelView` / `TunnelFormState`
// etc. without an extra `state::` / `form::`.
pub use form::{TunnelFormOutput, TunnelFormState, TunnelFormView};
pub use state::{TunnelRegistry, TunnelView};

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crate::app::CrabportApp;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::components::group_header::group_header;
use crate::motion::{DURATION_FAST, DURATION_MODERATE, EASE_STANDARD, RADIUS_MD, RADIUS_SM};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};
use crate::views::sessions::ConnectionHost;

use crabport_core::credential::{GroupEntry, GroupKind, TunnelKind};

/// Sentinel id used for the virtual "Favorites" group in collapse state.
/// Uses `i64::MAX` so it can never collide with a real group id.
const FAVORITES_GROUP_ID: i64 = i64::MAX;

/// Color accents for the kind badge (subtle tint, not the full primary
/// blue). Read live from the theme so a preset switch recolors the
/// badges too.
fn kind_local_color() -> u32 {
    term_blue()
}
fn kind_remote_color() -> u32 {
    term_magenta()
}
fn kind_dynamic_color() -> u32 {
    term_yellow()
}
fn status_running_color() -> u32 {
    term_green()
}
fn status_stopped_color() -> u32 {
    text_muted()
}

/// Tunnels sidebar view. Holds its own hover state so the action buttons can
/// fade in with easing when the row is hovered — without polluting
/// `CrabportApp` state.
pub struct TunnelsView {
    /// The tunnel row currently being hovered, if any. Keyed by
    /// `(id, is_favorite_copy)` so the favorites copy of an item and its
    /// real-group copy don't share hover state (they'd otherwise
    /// cross-highlight because both match the same id).
    hovered_tunnel_id: Option<(i64, bool)>,
    /// The tunnel row that triggered the currently-open context menu, if any.
    /// While set, that row stays highlighted in the hover color even though
    /// the mouse has moved to the overlay.
    context_menu_tunnel_id: Option<(i64, bool)>,
    // External data pushed in before each render.
    tunnels: Vec<TunnelView>,
    hosts: Vec<ConnectionHost>,
    /// Held for the context-menu/alert wiring (mirrors `SessionsView`).
    app: Entity<CrabportApp>,
    // Global context menu host, used for the right-click menu on each row.
    context_menu: Option<Entity<ContextMenuController>>,
    // Global alert dialog host, used for the delete-confirmation prompt.
    alert_controller: Option<Entity<AlertController>>,
    // Callbacks
    on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    on_start: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_stop: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    // The tunnel form dialog state, pushed in before each render. When
    // `Some` and `is_open()`, the view renders the `TunnelFormView` overlay
    // on top of the list — mirroring how `SessionsView` renders
    // `ConnectionFormView`.
    form_state: Option<TunnelFormState>,
    /// Per-render snapshot of tunnel groups (loaded from the store in
    // `render`). Kept on the struct so the context-menu builder can reach
    // the same list without re-querying the store.
    groups: Vec<GroupEntry>,
    /// Collapsed group ids (collapsible group headers). Ungrouped tunnels
    // are always shown.
    collapsed_groups: HashSet<i64>,
    /// Shared inline group-rename state (id + InputState).
    group_rename: GroupRenameState,
}

impl TunnelsView {
    pub fn new(app: Entity<CrabportApp>) -> Self {
        Self {
            hovered_tunnel_id: None,
            context_menu_tunnel_id: None,
            tunnels: Vec::new(),
            hosts: Vec::new(),
            app,
            context_menu: None,
            alert_controller: None,
            on_new: None,
            on_start: None,
            on_stop: None,
            on_edit: None,
            on_remove: None,
            form_state: None,
            groups: Vec::new(),
            collapsed_groups: HashSet::new(),
            group_rename: GroupRenameState::new(),
        }
    }

    /// Push the latest external state into the view before render.
    pub fn set_state(
        &mut self,
        tunnels: Vec<TunnelView>,
        hosts: Vec<ConnectionHost>,
        on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        on_start: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_stop: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        form_state: Option<TunnelFormState>,
        cx: &mut Context<Self>,
    ) {
        // Clear stale hover if the tunnel disappeared.
        if let Some((id, _)) = self.hovered_tunnel_id
            && !tunnels.iter().any(|t| t.id == id)
        {
            self.hovered_tunnel_id = None;
        }
        self.tunnels = tunnels;
        self.hosts = hosts;
        self.on_new = on_new;
        self.on_start = on_start;
        self.on_stop = on_stop;
        self.on_edit = on_edit;
        self.on_remove = on_remove;
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        self.form_state = form_state;
        // Note: do NOT call cx.notify() here — set_state is invoked every
        // render from render_content, so notifying would cause an infinite
        // loop. The TunnelsView re-renders naturally because its parent
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

impl GroupRenameView for TunnelsView {
    fn group_rename(&mut self) -> &mut GroupRenameState {
        &mut self.group_rename
    }

    fn app_entity(&self) -> &Entity<CrabportApp> {
        &self.app
    }
}

impl Render for TunnelsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let tunnels = self.tunnels.clone();
        let hosts = self.hosts.clone();
        let on_new = self.on_new.clone();
        let on_start = self.on_start.clone();
        let on_stop = self.on_stop.clone();
        let on_edit = self.on_edit.clone();
        let on_remove = self.on_remove.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();
        let hovered_tunnel_id = self.hovered_tunnel_id;
        let form_state = self.form_state.clone();
        let app = self.app.clone();
        // Tunnels only run over SSH (the underlying driver is an SSH
        // client — see `crabport-ssh`). Filter the host list so the
        // tunnel form's host dropdown only offers SSH hosts, instead of
        // also surfacing Telnet / Serial entries that can't actually back
        // a tunnel.
        let form_hosts = self
            .hosts
            .iter()
            .filter(|h| h.kind == crate::views::sessions::ConnectionKind::SSH)
            .cloned()
            .collect::<Vec<_>>();

        // Load tunnel groups from the store (ordered by sort_order, id).
        let groups = AppState::store(_cx)
            .lock()
            .groups(GroupKind::Tunnel)
            .unwrap_or_default();
        self.groups = groups.clone();

        // If the global context menu is no longer active, clear the
        // "menu-triggering row" highlight.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(_cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.context_menu_tunnel_id = None;
        }
        let context_menu_tunnel_id = self.context_menu_tunnel_id;
        let renaming_group_id = self.group_rename.renaming_group_id;
        let rename_input = self.group_rename.rename_input.clone();
        let collapsed_groups = self.collapsed_groups.clone();
        let entity = _cx.entity().downgrade();

        // Partition tunnels into ungrouped + per-group buckets. The store's
        // `tunnels()` query already sorts `favorite DESC, id`, so each bucket
        // inherits that order (favorites float to the top within the bucket).
        let mut ungrouped: Vec<&TunnelView> = Vec::new();
        let mut grouped: std::collections::HashMap<i64, Vec<&TunnelView>> =
            std::collections::HashMap::new();
        for t in &tunnels {
            match t.group_id {
                Some(gid) => grouped.entry(gid).or_default().push(t),
                None => ungrouped.push(t),
            }
        }

        // Favorites bucket: every tunnel with `favorite == true`, regardless
        // of which group it belongs to. This is a *virtual* group — the
        // items still appear in their real groups below.
        let favorites: Vec<&TunnelView> = tunnels.iter().filter(|t| t.favorite).collect();
        let favorites_collapsed = self.collapsed_groups.contains(&FAVORITES_GROUP_ID);

        // Build the row elements for a slice of tunnels. Returns a Vec so it
        // can be spliced into either the ungrouped section or a group body.
        let build_rows = |tunnels_slice: &[&TunnelView],
                          hosts: &[ConnectionHost],
                          groups: &[GroupEntry],
                          on_start: &Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
                          on_stop: &Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
                          on_edit: &Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
                          on_remove: &Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
                          context_menu: &Option<Entity<ContextMenuController>>,
                          alert_controller: &Option<Entity<AlertController>>,
                          hovered_tunnel_id: Option<(i64, bool)>,
                          context_menu_tunnel_id: Option<(i64, bool)>,
                          entity: WeakEntity<TunnelsView>,
                          app: Entity<CrabportApp>,
                          is_favorite_copy: bool|
         -> Vec<AnyElement> {
            tunnels_slice
                .iter()
                .map(|t| {
                    let tunnel = (*t).clone();
                    let host_name = hosts
                        .iter()
                        .find(|h| h.id == tunnel.host_id)
                        .map(|h| h.name.clone())
                        .unwrap_or_else(|| "?".to_string());
                    let on_start = on_start.clone();
                    let on_stop = on_stop.clone();
                    let on_edit = on_edit.clone();
                    let on_remove = on_remove.clone();
                    let context_menu = context_menu.clone();
                    let alert_controller = alert_controller.clone();
                    let is_hovered = hovered_tunnel_id == Some((tunnel.id, is_favorite_copy));
                    let force_highlight =
                        context_menu_tunnel_id == Some((tunnel.id, is_favorite_copy));
                    let groups = groups.to_vec();
                    let app_for_row = app.clone();
                    let entity_for_row = entity.clone();

                    tunnel_row(
                        &tunnel,
                        &host_name,
                        is_favorite_copy,
                        is_hovered,
                        force_highlight,
                        entity_for_row,
                        context_menu,
                        alert_controller,
                        groups,
                        app_for_row,
                        move |w, cx| {
                            if let Some(ref cb) = on_start {
                                cb(tunnel.id, w, cx);
                            }
                        },
                        move |w, cx| {
                            if let Some(ref cb) = on_stop {
                                cb(tunnel.id, w, cx);
                            }
                        },
                        move |w, cx| {
                            if let Some(ref cb) = on_edit {
                                cb(tunnel.id, w, cx);
                            }
                        },
                        move |w, cx| {
                            if let Some(ref cb) = on_remove {
                                cb(tunnel.id, w, cx);
                            }
                        },
                    )
                    .into_any_element()
                })
                .collect()
        };

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
                            .child(t!("sidebar.tunnels").to_string()),
                    )
                    .child(
                        Button::new("tunnels-new-btn")
                            .primary()
                            .icon("icons/plus.svg")
                            .w_auto()
                            .px_2()
                            .child(t!("tunnels.new_button").to_string())
                            .on_click(move |_e, w, cx| {
                                if let Some(ref cb) = on_new {
                                    cb(w, cx);
                                }
                            }),
                    ),
            )
            // --- Separator ---
            .child(div().h_px().bg(rgb(border())).mx_4())
            // --- Tunnels list (or empty state) ---
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_4()
                    .py_2()
                    .when_else(
                        tunnels.is_empty(),
                        |el| {
                            el.flex().items_center().justify_center().child(
                                div()
                                    .text_color(rgb(text_muted()))
                                    .text_sm()
                                    .child(t!("tunnels.empty").to_string()),
                            )
                        },
                        |el| {
                            el.flex()
                                .flex_col()
                                .gap_1()
                                // --- Ungrouped tunnels (flat, favorites first) ---
                                .children(build_rows(
                                    &ungrouped,
                                    &hosts,
                                    &groups,
                                    &on_start,
                                    &on_stop,
                                    &on_edit,
                                    &on_remove,
                                    &context_menu,
                                    &alert_controller,
                                    hovered_tunnel_id,
                                    context_menu_tunnel_id,
                                    entity.clone(),
                                    app.clone(),
                                    false,
                                ))
                                // --- Virtual Favorites group (all starred items) ---
                                .when(!favorites.is_empty(), |el| {
                                    let fav_count = favorites.len();
                                    let header = group_header(
                                        "tunnel",
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
                                    let fav_rows = build_rows(
                                        &favorites,
                                        &hosts,
                                        &groups,
                                        &on_start,
                                        &on_stop,
                                        &on_edit,
                                        &on_remove,
                                        &context_menu,
                                        &alert_controller,
                                        hovered_tunnel_id,
                                        context_menu_tunnel_id,
                                        entity.clone(),
                                        app.clone(),
                                        true,
                                    );

                                    let body_id = ElementId::Name(
                                        format!("tunnel-group-body-{}", FAVORITES_GROUP_ID).into(),
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
                                                el.h(px(fav_count as f32 * 86.0 - 4.0)).opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(fav_rows)
                                        .into_any_element();

                                    el.child(header).child(body)
                                })
                                // --- Grouped tunnels, one section per group ---
                                .children(groups.iter().filter_map(|g| {
                                    let members = grouped.get(&g.id)?;
                                    if members.is_empty() {
                                        return None;
                                    }
                                    let collapsed = collapsed_groups.contains(&g.id);
                                    let gid = g.id;
                                    let group_name = g.name.clone();
                                    let group_favorite = g.favorite;
                                    let group = g.clone();
                                    let member_count = members.len();
                                    let mut section = div().flex().flex_col().gap_1();
                                    let header = group_header(
                                        "tunnel",
                                        gid,
                                        group_name.clone(),
                                        member_count,
                                        collapsed,
                                        group_favorite,
                                        false,
                                        Rc::new({
                                            let header_entity = entity.clone();
                                            move |_w, cx| {
                                                let _ = header_entity.update(cx, |view, cx| {
                                                    if view.collapsed_groups.contains(&gid) {
                                                        view.collapsed_groups.remove(&gid);
                                                    } else {
                                                        view.collapsed_groups.insert(gid);
                                                    }
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                        None,
                                        Some({
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let app = app.clone();
                                            let group_for_menu = group.clone();
                                            let entity = entity.clone();
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
                                                        t!("tunnels.rename_group").to_string(),
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
                                                            t!("tunnels.delete_group").to_string(),
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
                                                                                    "tunnels.delete_group"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                description: Some(
                                                                                    t!(
                                                                                        "tunnels.delete_group_prompt",
                                                                                        name = name.as_str()
                                                                                    )
                                                                                    .to_string()
                                                                                    .into(),
                                                                                ),
                                                                                confirm_label: t!(
                                                                                    "tunnels.delete"
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
                                                                                            app.remove_group(gid, crabport_core::credential::GroupKind::Tunnel, cx);
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
                                    section = section.child(header);
                                    // Build rows always (even when collapsed)
                                    // so the expand animation has content to
                                    // reveal.
                                    let rows = build_rows(
                                        members,
                                        &hosts,
                                        &groups,
                                        &on_start,
                                        &on_stop,
                                        &on_edit,
                                        &on_remove,
                                        &context_menu,
                                        &alert_controller,
                                        hovered_tunnel_id,
                                        context_menu_tunnel_id,
                                        entity.clone(),
                                        app.clone(),
                                        false,
                                    );
                                    let body_id = ElementId::Name(
                                        format!("tunnel-group-body-{}", gid).into(),
                                    );
                                    let body = div()
                                        .id(body_id.clone())
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_hidden()
                                        .with_transition(body_id)
                                        .transition_when_else(
                                            !collapsed,
                                            DURATION_MODERATE,
                                            EASE_STANDARD,
                                            move |el| {
                                                el.h(px(member_count as f32 * 86.0 - 4.0))
                                                    .opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(rows);
                                    section = section.child(body);
                                    Some(section.into_any_element())
                                }))
                        },
                    ),
            )
            // --- Tunnel form overlay (create/edit) ---
            // Mirrors `SessionsView`'s rendering of `ConnectionFormView`: when
            // the form state is `Some`, render the overlay on top of the list.
            .when_some(form_state, move |el, state| {
                el.child(TunnelFormView::new(&state, app, form_hosts, _cx))
            })
    }
}

// ---------------------------------------------------------------------------
// Legacy free-function render path
// ---------------------------------------------------------------------------
//
// `render_content` in `layouts/content.rs` still calls this for the
// `SidebarItem::Tunnels` arm. It will be updated separately to construct a
// `TunnelsView` entity and push state via `set_state`. Until then, this shim
// renders a minimal placeholder (header + empty state) so the crate keeps
// compiling.

#[allow(dead_code)]
pub fn render_tunnels_view(on_new: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .relative()
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
                        .child(t!("sidebar.tunnels").to_string()),
                )
                .child(
                    Button::new("tunnels-new-btn")
                        .primary()
                        .icon("icons/plus.svg")
                        .w_auto()
                        .px_2()
                        .child(t!("tunnels.new_button").to_string())
                        .on_click(move |_e, w, cx| {
                            on_new(w, cx);
                        }),
                ),
        )
        .child(div().h_px().bg(rgb(border())).mx_4())
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_4()
                .py_2()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(rgb(text_muted()))
                        .text_sm()
                        .child(t!("tunnels.empty").to_string()),
                ),
        )
}

// ---------------------------------------------------------------------------
// Tunnel row
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn tunnel_row(
    tunnel: &TunnelView,
    host_name: &str,
    is_favorite_copy: bool,
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<TunnelsView>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert_controller: Option<Entity<AlertController>>,
    groups: Vec<GroupEntry>,
    app: Entity<CrabportApp>,
    on_start: impl Fn(&mut Window, &mut App) + 'static,
    on_stop: impl Fn(&mut Window, &mut App) + 'static,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    on_remove: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The favorites bucket renders the same tunnel a second time (the
    // item also appears under its real group below). Each instance needs
    // a distinct transition id, otherwise hover state is shared across
    // both and they animate in lockstep. Suffix "-fav" disambiguates the
    // copy.
    let row_id = ElementId::Name(
        format!(
            "tunnel-row-{}{}",
            tunnel.id,
            if is_favorite_copy { "-fav" } else { "" }
        )
        .into(),
    );

    let tunnel_id = tunnel.id;
    let tunnel_running = tunnel.running;
    let tunnel_borrowed = tunnel.borrowed_tab_id.is_some();
    let tunnel_favorite = tunnel.favorite;
    let is_highlighted = is_hovered || force_highlight;

    // Kind badge label + accent color + secondary address line.
    let (kind_letter, kind_label, kind_color) = match tunnel.kind {
        TunnelKind::Local => (
            "L",
            t!("tunnels.kind_local").to_string(),
            kind_local_color(),
        ),
        TunnelKind::Remote => (
            "R",
            t!("tunnels.kind_remote").to_string(),
            kind_remote_color(),
        ),
        TunnelKind::Dynamic => (
            "D",
            t!("tunnels.kind_dynamic").to_string(),
            kind_dynamic_color(),
        ),
    };
    let bind_display = if tunnel.bind_addr.is_empty() {
        format!("*:{}", tunnel.bind_port)
    } else {
        format!("{}:{}", tunnel.bind_addr, tunnel.bind_port)
    };
    let address_line = match tunnel.kind {
        TunnelKind::Local | TunnelKind::Remote => format!(
            "{}  {} → {}:{}",
            kind_letter, bind_display, tunnel.target_host, tunnel.target_port
        ),
        TunnelKind::Dynamic => format!("{}  {} (SOCKS5)", kind_letter, bind_display),
    };

    // Status pill content.
    let (status_dot, status_text) = if tunnel_running {
        let suffix = if tunnel_borrowed {
            t!("tunnels.borrowed").to_string()
        } else {
            t!("tunnels.owned").to_string()
        };
        (
            status_running_color(),
            format!("{} ({})", t!("tunnels.running").to_string(), suffix),
        )
    } else {
        (status_stopped_color(), t!("tunnels.stopped").to_string())
    };

    // Wrap the action callbacks in Rc so they can be cloned into both the
    // inline buttons and the context-menu items.
    let on_start_rc = Rc::new(on_start);
    let on_stop_rc = Rc::new(on_stop);
    let on_edit_rc = Rc::new(on_edit);
    let on_remove_rc = Rc::new(on_remove);

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
        // Right-click context menu: Favorite, Move-to-Group, Start/Stop,
        // Edit, Delete.
        .on_mouse_down(MouseButton::Right, {
            let on_edit = on_edit_rc.clone();
            let on_remove = on_remove_rc.clone();
            let on_start = on_start_rc.clone();
            let on_stop = on_stop_rc.clone();
            let entity = entity.clone();
            // Clone these here so the closure captures fresh copies, leaving
            // the originals available for the inline action buttons below.
            let alert_controller = alert_controller.clone();
            let groups_for_menu = groups.clone();
            let app_for_menu = app.clone();
            move |event, _w, cx| {
                let Some(ref cm) = context_menu else {
                    return;
                };
                // Mark this row as the menu-triggering row so it keeps the
                // hover background while the overlay is up.
                let _ = entity.update(cx, |view, cx| {
                    view.context_menu_tunnel_id = Some((tunnel_id, is_favorite_copy));
                    cx.notify();
                });
                let pos = event.position;
                let on_edit = on_edit.clone();
                let on_remove = on_remove.clone();
                let on_start = on_start.clone();
                let on_stop = on_stop.clone();
                let alert_controller = alert_controller.clone();
                let _groups = groups_for_menu.clone();
                let app = app_for_menu.clone();
                cm.update(cx, |c, cx| {
                    // Build the contextual Start/Stop item based on current
                    // running state.
                    let toggle_item = if tunnel_running {
                        ContextMenuItem::new(t!("tunnels.stop").to_string(), {
                            let on_stop = on_stop.clone();
                            move |w, cx| {
                                on_stop(w, cx);
                            }
                        })
                    } else {
                        ContextMenuItem::new(t!("tunnels.start").to_string(), {
                            let on_start = on_start.clone();
                            move |w, cx| {
                                on_start(w, cx);
                            }
                        })
                    }
                    .divider_after();

                    // Favorite toggle.
                    let favorite_label = if tunnel_favorite {
                        t!("tunnels.unfavorite").to_string()
                    } else {
                        t!("tunnels.favorite").to_string()
                    };
                    let favorite_item = ContextMenuItem::new(favorite_label, {
                        let app = app.clone();
                        move |_w, cx| {
                            app.update(cx, |app, cx| {
                                app.toggle_tunnel_favorite(tunnel_id, cx);
                            });
                        }
                    })
                    .divider_after();

                    let mut items = vec![toggle_item, favorite_item];
                    items.push(
                        ContextMenuItem::new(t!("tunnels.edit").to_string(), {
                            let on_edit = on_edit.clone();
                            move |w, cx| {
                                on_edit(w, cx);
                            }
                        })
                        // Disable Edit while the tunnel is running — editing the
                        // bind/target of a live forward would silently
                        // diverge from the active session, so require Stop
                        // first.
                        .disabled(tunnel_running),
                    );
                    items.push(
                        ContextMenuItem::new(t!("tunnels.delete").to_string(), {
                            let on_remove = on_remove.clone();
                            let alert_controller = alert_controller.clone();
                            move |_w, cx| {
                                let Some(ref ac) = alert_controller else {
                                    return;
                                };
                                let on_remove = on_remove.clone();
                                ac.update(cx, |c, cx| {
                                    c.show(
                                        AlertState {
                                            severity: AlertSeverity::Danger,
                                            title: t!("tunnels.delete_confirm_title")
                                                .to_string()
                                                .into(),
                                            description: Some(
                                                t!("tunnels.delete_confirm_msg").to_string().into(),
                                            ),
                                            confirm_label: t!("tunnels.delete").to_string().into(),
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
                        .danger(true)
                        // Disable Delete while the tunnel is running —
                        // deleting a live forward would leave the underlying
                        // SSH session/channel dangling. Require Stop first.
                        .disabled(tunnel_running),
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
        // Double-click toggles start/stop. Must be on the pre-transition
        // `div` (the `AnimatedWrapper` produced by `with_transition`
        // below doesn't expose `on_mouse_down`).
        .on_mouse_down(MouseButton::Left, {
            let on_start = on_start_rc.clone();
            let on_stop = on_stop_rc.clone();
            move |event, w, cx| {
                if event.click_count >= 2 {
                    if tunnel_running {
                        on_stop(w, cx);
                    } else {
                        on_start(w, cx);
                    }
                }
            }
        })
        // Track hover of the whole row so the background color eases in.
        .with_transition(row_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                if *hovered {
                    view.hovered_tunnel_id = Some((tunnel_id, is_favorite_copy));
                } else if view.hovered_tunnel_id == Some((tunnel_id, is_favorite_copy)) {
                    view.hovered_tunnel_id = None;
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
        // --- Left: star toggle + kind badge + tunnel info ---
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_2()
                .min_w_0()
                .flex_1()
                // Kind badge (single letter, color-coded)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_5()
                        .rounded(RADIUS_SM)
                        .bg(rgba((kind_color << 8) | 0x22))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(kind_color))
                        .child(kind_letter.to_string()),
                )
                // Name + address + host
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(text_primary()))
                                .child(tunnel.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(address_line),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(format!("{} · {}", host_name, kind_label)),
                        ),
                ),
        )
        // --- Right: status pill + favorite star ---
        // Start/Stop/Edit/Delete live in the right-click context menu
        // (see `on_mouse_down` above). Double-click the row toggles
        // start/stop. The star sits at the far right; it fades in on hover
        // and stays visible (yellow) when already favorited.
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(div().size_2().rounded_full().bg(rgb(status_dot)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(status_text),
                        ),
                )
                .child({
                    let app = app.clone();
                    let star_id = ElementId::Name(format!("tunnel-star-{}", tunnel.id).into());
                    // Star is visible when the row is hovered OR the tunnel is
                    // already favorited (so the user can see + unstar).
                    let star_visible = is_highlighted || tunnel_favorite;
                    div()
                        .id(star_id.clone())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("icons/star.svg").size_4().text_color(rgb(
                            if tunnel_favorite {
                                term_yellow()
                            } else {
                                text_muted()
                            },
                        )))
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
                                app.toggle_tunnel_favorite(tunnel_id, cx);
                            });
                        })
                }),
        )
}
