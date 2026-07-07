use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::InteractiveElementExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::time::Duration;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};

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
    pub kind: crate::views::hosts::ConnectionKind,
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
pub struct HostsView {
    /// The host row currently being hovered, if any.
    hovered_host_id: Option<i64>,
    /// The host row that triggered the currently-open context menu, if any.
    /// While set, that row stays highlighted in the hover color even though
    /// the mouse has moved to the overlay.
    context_menu_host_id: Option<i64>,
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
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
}

impl HostsView {
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
            on_edit: None,
            on_remove: None,
        }
    }

    /// Push the latest external state into the view before render.
    pub fn set_state(
        &mut self,
        hosts: Vec<ConnectionHost>,
        form_state: Option<ConnectionFormState>,
        on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        on_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        on_remove: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        cx: &mut Context<Self>,
    ) {
        // Clear stale hover if the host disappeared.
        if let Some(id) = self.hovered_host_id
            && !hosts.iter().any(|h| h.id == id)
        {
            self.hovered_host_id = None;
        }
        self.hosts = hosts;
        self.form_state = form_state;
        self.on_new = on_new;
        self.on_connect = on_connect;
        self.on_edit = on_edit;
        self.on_remove = on_remove;
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        // Note: do NOT call cx.notify() here — set_state is invoked every
        // render from render_content, so notifying would cause an infinite
        // loop. The HostsView re-renders naturally because its parent
        // (CrabportApp) re-renders.
        let _ = cx;
    }
}

impl Render for HostsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.hosts.clone();
        let form_state = self.form_state.clone();
        let app = self.app.clone();
        let on_new = self.on_new.clone();
        let on_connect = self.on_connect.clone();
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
                            el.flex().flex_col().gap_1().children(hosts.iter().map(|h| {
                                let host = h.clone();
                                let on_connect = on_connect.clone();
                                let on_edit = on_edit.clone();
                                let on_remove = on_remove.clone();
                                let context_menu = context_menu.clone();
                                let alert_controller = alert_controller.clone();
                                let is_hovered = hovered_host_id == Some(h.id);
                                let force_highlight = context_menu_host_id == Some(h.id);
                                let entity = _cx.entity().downgrade();

                                host_row(
                                    &host,
                                    is_hovered,
                                    force_highlight,
                                    entity,
                                    context_menu,
                                    alert_controller,
                                    app.clone(),
                                    groups.clone(),
                                    on_connect.clone(),
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
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<HostsView>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert_controller: Option<Entity<AlertController>>,
    app: Entity<CrabportApp>,
    groups: Vec<crabport_core::credential::GroupEntry>,
    on_connect: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    on_remove: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let row_id = ElementId::Name(format!("host-row-{}", host.id).into());
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
        .rounded_md()
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
            let app = app.clone();
            let groups = groups.clone();
            move |event, _w, cx| {
                let Some(ref cm) = context_menu else {
                    return;
                };
                // Mark this row as the menu-triggering row so it keeps the
                // hover background while the menu is up.
                let _ = entity.update(cx, |view, cx| {
                    view.context_menu_host_id = Some(host_id);
                    cx.notify();
                });
                let pos = event.position;
                let on_edit = on_edit.clone();
                let on_remove = on_remove.clone();
                let on_connect = on_connect.clone();
                let app_for_menu = app.clone();
                let groups_for_menu = groups.clone();
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

                    // Move-to-Group section (mirrors snippet ctxmenu):
                    // disabled header, Ungrouped, one per group, New Group…
                    items.push(
                        ContextMenuItem::new(t!("hosts.move_to_group").to_string(), |_, _| {})
                            .disabled(true),
                    );
                    items.push(ContextMenuItem::new(t!("hosts.ungrouped").to_string(), {
                        let app = app_for_menu.clone();
                        move |_w, cx| {
                            app.update(cx, |app, cx| {
                                app.set_host_group(host_id, None, cx);
                            });
                        }
                    }));
                    for g in &groups_for_menu {
                        let gid = g.id;
                        let name = g.name.clone();
                        let app = app_for_menu.clone();
                        items.push(ContextMenuItem::new(name, move |_w, cx| {
                            app.update(cx, |app, cx| {
                                app.set_host_group(host_id, Some(gid), cx);
                            });
                        }));
                    }
                    items.push(
                        ContextMenuItem::new(t!("hosts.new_group").to_string(), {
                            let app = app_for_menu.clone();
                            move |_w, cx| {
                                app.update(cx, |app, cx| {
                                    app.open_group_form_for_create(
                                        crabport_core::credential::GroupKind::Host,
                                        cx,
                                    );
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
        // State lives in the HostsView entity itself.
        .with_transition(row_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                if *hovered {
                    view.hovered_host_id = Some(host_id);
                } else {
                    if view.hovered_host_id == Some(host_id) {
                        view.hovered_host_id = None;
                    }
                }
                cx.notify();
            });
        })
        .transition_when_else(
            is_highlighted,
            Duration::from_millis(120),
            Linear,
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
                .cursor_pointer()
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
                    Duration::from_millis(120),
                    Linear,
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
