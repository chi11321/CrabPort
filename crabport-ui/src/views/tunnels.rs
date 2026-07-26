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

use gpui::*;
use rust_i18n::t;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::context_menu::{
    ContextMenuController, ContextMenuItem, ContextMenuState, confirm_delete_item,
};
use crate::components::dialog::AlertController;
use crate::components::list_row::{favorite_star, interactive_row};
use crate::motion::RADIUS_SM;
use crate::views::collection::{GroupedListView, render_grouped_list};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};
use crate::views::sessions::ConnectionHost;

use crabport_core::credential::{GroupKind, TunnelKind};

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
    /// Collapsed group ids (collapsible group headers). Ungrouped tunnels
    /// are always shown.
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
            collapsed_groups: HashSet::new(),
            group_rename: GroupRenameState::new(),
        }
    }

    /// Push the latest external state into the view before render.
    #[allow(clippy::too_many_arguments)]
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
}

impl GroupRenameView for TunnelsView {
    fn group_rename(&mut self) -> &mut GroupRenameState {
        &mut self.group_rename
    }

    fn app_entity(&self) -> &Entity<CrabportApp> {
        &self.app
    }
}

impl GroupedListView for TunnelsView {
    type Item = TunnelView;

    const ID_PREFIX: &'static str = "tunnel";
    const MENU_NS: &'static str = "tunnels";
    const GROUP_KIND: GroupKind = GroupKind::Tunnel;
    const ROW_HEIGHT: f32 = 86.0;
    const SKIP_EMPTY_GROUPS: bool = true;
    const WRAP_SECTIONS: bool = true;
    const TITLE_KEY: &'static str = "sidebar.tunnels";
    const NEW_BUTTON_ID: &'static str = "tunnels-new-btn";
    const NEW_BUTTON_KEY: &'static str = "tunnels.new_button";
    const EMPTY_KEY: &'static str = "tunnels.empty";

    fn items(&self) -> &[TunnelView] {
        &self.tunnels
    }
    fn item_id(item: &TunnelView) -> i64 {
        item.id
    }
    fn item_group_id(item: &TunnelView) -> Option<i64> {
        item.group_id
    }
    fn item_favorite(item: &TunnelView) -> bool {
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
        self.hovered_tunnel_id
    }
    fn menu_row_state(&mut self) -> &mut Option<(i64, bool)> {
        &mut self.context_menu_tunnel_id
    }
    fn collapsed_groups(&mut self) -> &mut HashSet<i64> {
        &mut self.collapsed_groups
    }

    fn render_row(
        &self,
        tunnel: &TunnelView,
        is_favorite_copy: bool,
        is_hovered: bool,
        force_highlight: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tunnel_id = tunnel.id;
        let host_name = self
            .hosts
            .iter()
            .find(|h| h.id == tunnel.host_id)
            .map(|h| h.name.clone())
            .unwrap_or_else(|| "?".to_string());
        let on_start = self.on_start.clone();
        let on_stop = self.on_stop.clone();
        let on_edit = self.on_edit.clone();
        let on_remove = self.on_remove.clone();
        tunnel_row(
            tunnel,
            &host_name,
            is_favorite_copy,
            is_hovered,
            force_highlight,
            cx.entity().downgrade(),
            self.context_menu.clone(),
            self.alert_controller.clone(),
            self.app.clone(),
            move |w, cx| {
                if let Some(ref cb) = on_start {
                    cb(tunnel_id, w, cx);
                }
            },
            move |w, cx| {
                if let Some(ref cb) = on_stop {
                    cb(tunnel_id, w, cx);
                }
            },
            move |w, cx| {
                if let Some(ref cb) = on_edit {
                    cb(tunnel_id, w, cx);
                }
            },
            move |w, cx| {
                if let Some(ref cb) = on_remove {
                    cb(tunnel_id, w, cx);
                }
            },
        )
        .into_any_element()
    }

    fn render_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.form_state.as_ref()?;
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
        Some(TunnelFormView::new(state, self.app.clone(), form_hosts, cx).into_any_element())
    }
}

impl Render for TunnelsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_grouped_list(self, cx)
    }
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
    // double-click handler and the context-menu items.
    let on_start_rc = Rc::new(on_start);
    let on_stop_rc = Rc::new(on_stop);
    let on_edit_rc = Rc::new(on_edit);
    let on_remove_rc = Rc::new(on_remove);

    // --- Left: kind badge + tunnel info ---
    let info = div()
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
        )
        .into_any_element();

    // --- Right: status pill + favorite star ---
    // Start/Stop/Edit/Delete live in the right-click context menu.
    // Double-click the row toggles start/stop.
    let right = div()
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
        .child(favorite_star(
            "tunnel",
            tunnel_id,
            tunnel_favorite,
            is_highlighted || tunnel_favorite,
            {
                let app = app.clone();
                move |cx| {
                    app.update(cx, |app, cx| {
                        app.toggle_tunnel_favorite(tunnel_id, cx);
                    });
                }
            },
        ))
        .into_any_element();

    interactive_row(
        row_id,
        is_highlighted,
        entity.clone(),
        (tunnel_id, is_favorite_copy),
        |view| &mut view.hovered_tunnel_id,
        move |el| {
            // Right-click context menu: Start/Stop, Favorite, Edit, Delete.
            el.on_mouse_down(MouseButton::Right, {
                let on_edit = on_edit_rc.clone();
                let on_remove = on_remove_rc.clone();
                let on_start = on_start_rc.clone();
                let on_stop = on_stop_rc.clone();
                move |event, _w, cx| {
                    let Some(ref cm) = context_menu else {
                        return;
                    };
                    // Mark this row as the menu-triggering row so it keeps
                    // the hover background while the overlay is up.
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
                    let app = app.clone();
                    cm.update(cx, |c, cx| {
                        // Build the contextual Start/Stop item based on
                        // current running state.
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
                            confirm_delete_item(
                                t!("tunnels.delete").to_string(),
                                alert_controller.clone(),
                                || {
                                    (
                                        t!("tunnels.delete_confirm_title").to_string().into(),
                                        Some(t!("tunnels.delete_confirm_msg").to_string().into()),
                                        t!("tunnels.delete").to_string().into(),
                                    )
                                },
                                {
                                    let on_remove = on_remove.clone();
                                    Rc::new(move |w, cx| {
                                        on_remove(w, cx);
                                    })
                                },
                            )
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
            // doesn't expose `on_mouse_down`).
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
        },
        vec![info, right],
    )
}
