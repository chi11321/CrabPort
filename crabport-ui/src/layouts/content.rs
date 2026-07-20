use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crate::app::AppCtx;
use crate::app::{CrabportApp, SidebarItem, Tab, TabKind};
use crate::color::*;
use crate::components::dialog::{AlertSeverity, AlertState};
use crate::layouts::panel::{PanelCaps, render_panel};
use crate::layouts::tabbar::render_tab_bar;
use crate::motion::{DURATION_FAST, EASE_STANDARD, RADIUS_SM};
use crate::views::panel::PanelKind;
use crate::views::panel::sftp::SftpDragValue;
use crate::views::sessions::{ConnectionFormState, ConnectionHost};
use crate::views::terminal::TerminalView;
use crate::views::terminal::split::{SplitDir, SplitNode};
use crate::views::terminal::toolbar::{TerminalToolbarInput, render_terminal_toolbar};
use crabport_terminal::terminal::{RemoteMetrics, RemoteStatus};

/// Clone the active terminal's `Entity` so a callback can forward calls to
/// the backend without capturing a borrow on `terminal_views`.
fn terminal_entity(
    is_terminal: bool,
    active_tab: Option<&Tab>,
    terminal_views: &HashMap<u64, Entity<TerminalView>>,
) -> Option<Entity<TerminalView>> {
    if !is_terminal {
        return None;
    }
    active_tab
        .and_then(|tab| terminal_views.get(&tab.id))
        .cloned()
}

pub fn render_content(
    selected: SidebarItem,
    handle: &Entity<CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    terminal_views: &HashMap<u64, Entity<TerminalView>>,
    split_trees: &HashMap<u64, crate::views::terminal::split::SplitTree>,
    pane_views: &HashMap<u64, Entity<TerminalView>>,
    sftp_view: &Entity<crate::views::sftp::SftpTabView>,
    hosts: &[ConnectionHost],
    form_entity: Option<&ConnectionFormState>,
    // Active panel pane the user last selected (semantic identity, not a
    // positional index — see `PanelKind`). Read by the caller (which owns
    // the `CrabportApp` borrow) and passed in to avoid a nested
    // `handle.read_with` during render.
    panel_active_tab: PanelKind,
    // Whether the user has toggled the right-hand panel open for this tab.
    // The toolbar's panel toggle button flips this; `render_panel`'s
    // `with_transition` drives the slide animation. The caller (which owns
    // the `CrabportApp` borrow) pre-reads it to avoid a nested
    // `handle.read_with` during render.
    panel_open: bool,
    // Pre-read by the caller (which owns the `CrabportApp` borrow) to avoid
    // a nested `handle.read_with` during render — same reason as
    // `panel_active_tab`.
    tunnel_list: Vec<crate::views::tunnels::TunnelView>,
    tunnel_form_state: Option<crate::views::tunnels::TunnelFormState>,
    snippet_form_state: Option<crate::views::snippets::SnippetFormState>,
    // Current panel width in px (either the live drag value or the persisted
    // config value). Drives the panel's rendered width.
    panel_width: f32,
    // Whether a panel resize drag is in progress.
    panel_dragging: bool,
    ctx: &AppCtx,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    // Unpack the shared context once — every field is a cheap handle/Arc.
    let sftp_panel = &ctx.sftp_panel;
    let snippets_panel = &ctx.snippets_panel;
    let history_panel = &ctx.history_panel;
    let tunnels_panel = &ctx.tunnels_panel;
    let sessions_view = &ctx.sessions_view;
    let snippets_view = &ctx.snippets_view;
    let tunnels_view = &ctx.tunnels_view;
    let context_menu = &ctx.context_menu;
    let alert_controller = &ctx.alert;
    let active_tab = tabs.iter().find(|t| t.id == active_tab_id);
    // Filter the tunnel list for the panel down to only the tunnels that
    // belong to the active terminal's host. A local PTY tab (`host_id` =
    // `None`) has no host → empty list; an SSH/Telnet tab shows only its own
    // host's tunnels. The full-page TunnelsView (SidebarItem::Tunnels arm
    // below) consumes the original unfiltered `tunnel_list`.
    let active_host_id = active_tab
        .and_then(|tab| terminal_views.get(&tab.id))
        .and_then(|entity| entity.read_with(cx, |view, _cx| view.host_id()));
    let tunnel_list_for_panel: Vec<crate::views::tunnels::TunnelView> = tunnel_list
        .iter()
        .filter(|t| Some(t.host_id) == active_host_id)
        .cloned()
        .collect();
    let handle_c = handle.clone();
    let on_close: Rc<dyn Fn(u64, &mut Window, &mut App)> = Rc::new(move |id, _w, cx| {
        handle_c.update(cx, |app, cx| {
            app.close_active_pane_or_tab(id, cx);
        });
    });

    let app_handle = handle.clone();
    let on_new = move |w: &mut Window, cx: &mut App| {
        app_handle.update(cx, |app, cx| {
            app.open_connection_form(w, cx);
        });
    };

    let view: AnyElement = match active_tab.map(|t| t.kind) {
        Some(TabKind::Home) => {
            match selected {
                SidebarItem::Sessions => {
                    let app_handle = handle.clone();
                    let on_connect = move |host_id: i64, _w: &mut Window, cx: &mut App| {
                        app_handle.update(cx, |app, cx| {
                            app.connect_to_host(host_id, cx);
                        });
                    };
                    let app_handle_sftp = handle.clone();
                    let on_sftp_connect = move |host_id: i64, w: &mut Window, cx: &mut App| {
                        app_handle_sftp.update(cx, |app, cx| {
                            app.switch_sftp_panel_host(host_id, w, cx);
                        });
                    };
                    let app_handle_edit = handle.clone();
                    let on_edit = move |host_id: i64, w: &mut Window, cx: &mut App| {
                        app_handle_edit.update(cx, |app, cx| {
                            app.edit_host(host_id, w, cx);
                        });
                    };
                    let app_handle_remove = handle.clone();
                    let on_remove = move |host_id: i64, _w: &mut Window, cx: &mut App| {
                        app_handle_remove.update(cx, |app, cx| {
                            app.remove_host(host_id, cx);
                        });
                    };

                    let on_new_rc: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_new);
                    let on_connect_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_connect);
                    let on_sftp_connect_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> =
                        Rc::new(on_sftp_connect);
                    let on_edit_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_edit);
                    let on_remove_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_remove);

                    sessions_view.update(cx, |view, cx| {
                        view.set_state(
                            hosts.to_vec(),
                            form_entity.cloned(),
                            Some(on_new_rc),
                            Some(on_connect_rc),
                            Some(on_sftp_connect_rc),
                            Some(on_edit_rc),
                            Some(on_remove_rc),
                            context_menu.clone(),
                            alert_controller.clone(),
                            cx,
                        );
                    });

                    sessions_view.clone().into_any_element()
                }
                SidebarItem::Tunnels => {
                    let app_handle = handle.clone();
                    let on_new = move |w: &mut Window, cx: &mut App| {
                        app_handle.update(cx, |app, cx| {
                            app.open_tunnel_form_for_create(w, cx);
                        });
                    };
                    let app_handle_start = handle.clone();
                    let on_start = move |id: i64, w: &mut Window, cx: &mut App| {
                        app_handle_start.update(cx, |app, cx| {
                            app.start_tunnel_owned(id, w, cx);
                        });
                    };
                    let app_handle_stop = handle.clone();
                    let on_stop = move |id: i64, _w: &mut Window, cx: &mut App| {
                        app_handle_stop.update(cx, |app, cx| {
                            app.stop_tunnel(id, cx);
                        });
                    };
                    let app_handle_edit = handle.clone();
                    let on_edit = move |id: i64, w: &mut Window, cx: &mut App| {
                        app_handle_edit.update(cx, |app, cx| {
                            app.open_tunnel_form_for_edit(id, w, cx);
                        });
                    };
                    let app_handle_remove = handle.clone();
                    let on_remove = move |id: i64, _w: &mut Window, cx: &mut App| {
                        app_handle_remove.update(cx, |app, cx| {
                            app.remove_tunnel(id, cx);
                        });
                    };

                    let on_new_rc: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_new);
                    let on_start_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_start);
                    let on_stop_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_stop);
                    let on_edit_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_edit);
                    let on_remove_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_remove);

                    tunnels_view.update(cx, |view, cx| {
                        view.set_state(
                            tunnel_list,
                            hosts.to_vec(),
                            Some(on_new_rc),
                            Some(on_start_rc),
                            Some(on_stop_rc),
                            Some(on_edit_rc),
                            Some(on_remove_rc),
                            context_menu.clone(),
                            alert_controller.clone(),
                            tunnel_form_state,
                            cx,
                        );
                    });

                    tunnels_view.clone().into_any_element()
                }
                SidebarItem::Snippets => {
                    // Load snippets from the Store and push into the view.
                    let store = crate::app_state::AppState::store(cx);
                    let rows = if let Ok(snippets) = store.lock().snippets() {
                        snippets
                            .into_iter()
                            .map(|s| crate::views::snippets::SnippetRow {
                                id: s.id,
                                name: s.name,
                                command: s.command,
                                favorite: s.favorite,
                                group_id: s.group_id,
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    // Wire New / Edit callbacks to the app's snippet-form
                    // methods (mirrors the Tunnels arm).
                    let app_handle = handle.clone();
                    let on_new = move |w: &mut Window, cx: &mut App| {
                        app_handle.update(cx, |app, cx| {
                            app.open_snippet_form_for_create(w, cx);
                        });
                    };
                    let app_handle_edit = handle.clone();
                    let on_edit = move |id: i64, w: &mut Window, cx: &mut App| {
                        app_handle_edit.update(cx, |app, cx| {
                            app.open_snippet_form_for_edit(id, w, cx);
                        });
                    };
                    let on_new_rc: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_new);
                    let on_edit_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_edit);
                    snippets_view.update(cx, |view, cx| {
                        view.set_state(
                            rows,
                            context_menu.clone(),
                            alert_controller.clone(),
                            Some(on_new_rc),
                            Some(on_edit_rc),
                            snippet_form_state,
                            cx,
                        );
                    });
                    snippets_view.clone().into_any_element()
                }
                SidebarItem::History => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(rgb(text_muted()))
                            .child(selected.label().to_string()),
                    )
                    .into_any_element(),
            }
        }
        Some(TabKind::Terminal) => {
            if let Some(tab) = active_tab {
                let tab_id = tab.id;
                // Clone the terminal entity so the on_drop handler can
                // trigger a download when the user drags an SFTP file row
                // onto the terminal area.
                let term_entity_for_drop = terminal_views.get(&tab_id).cloned();
                // If this tab has a split tree, render the panes recursively;
                // otherwise fall back to the single terminal view.
                let inner: AnyElement = if let Some(tree) = split_trees.get(&tab_id) {
                    render_split_node(&tree.root, tree.active_pane, tab_id, pane_views, handle)
                        .into_any_element()
                } else if let Some(terminal_entity) = terminal_views.get(&tab_id) {
                    div()
                        .size_full()
                        .key_context("Terminal")
                        .child(terminal_entity.clone())
                        .into_any_element()
                } else {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().text_color(rgb(text_muted())).child("Terminal"))
                        .into_any_element()
                };
                // Floating split-action buttons (top-right of the terminal
                // content area). "Split Right" / "Split Down" create a new
                // local-PTY pane; disabled when there's no terminal yet.
                let has_term =
                    terminal_views.get(&tab_id).is_some() || split_trees.get(&tab_id).is_some();
                let handle_split_r = handle.clone();
                let handle_split_d = handle.clone();
                let handle_panel = handle.clone();
                // Clone the download callback + terminal entity so the
                // on_drop handler can trigger a download when the user
                // drags an SFTP file row onto the terminal area.
                let on_download_for_drop = term_entity_for_drop.clone().map(|entity| {
                    move |drag: &SftpDragValue, cx: &mut App| {
                        let remote_path = drag.remote_path.clone();
                        let name = drag.name.clone();
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some(t!("sftp.download_prompt").to_string().into()),
                        });
                        let entity = entity.clone();
                        cx.spawn(async move |cx| {
                            let picked = match rx.await {
                                Ok(Ok(Some(mut paths))) => paths.pop(),
                                _ => None,
                            };
                            let Some(dest_dir) = picked else { return };
                            let local_path = dest_dir.join(&name);
                            let _ = cx.update(|cx| {
                                entity.read_with(cx, |view, _cx| {
                                    view.sftp_download(&remote_path, &local_path.to_string_lossy());
                                });
                            });
                        })
                        .detach();
                    }
                });
                div()
                    .size_full()
                    .relative()
                    .key_context("Terminal")
                    .child(inner)
                    .when_some(on_download_for_drop, |el, on_drop_cb| {
                        el.on_drop::<SftpDragValue>(move |drag, _w, cx| {
                            on_drop_cb(drag, cx);
                        })
                    })
                    .when(has_term, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_2()
                                .right_2()
                                .flex()
                                .flex_row()
                                .gap_1()
                                // Occlude so mouse-down on the split buttons
                                // doesn't fall through to the terminal pane
                                // underneath (which would re-focus that pane
                                // and make `split_active_pane` target the
                                // wrong pane).
                                .occlude()
                                .child(render_split_button(
                                    "term-split-right",
                                    "icons/panel-right.svg",
                                    t!("terminal.split_right").to_string(),
                                    ctx.tooltip.clone(),
                                    {
                                        let handle = handle_split_r.clone();
                                        move |_w, cx| {
                                            handle.update(cx, |app, cx| {
                                                app.split_active_pane(
                                                    crate::views::terminal::split::SplitDir::Vertical,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ))
                                .child(render_split_button(
                                    "term-split-down",
                                    "icons/panel-bottom.svg",
                                    t!("terminal.split_down").to_string(),
                                    ctx.tooltip.clone(),
                                    {
                                        let handle = handle_split_d.clone();
                                        move |_w, cx| {
                                            handle.update(cx, |app, cx| {
                                                app.split_active_pane(
                                                    crate::views::terminal::split::SplitDir::Horizontal,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                ))
                                .child(render_split_button(
                                    "term-toggle-panel",
                                    "icons/panel-right.svg",
                                    t!("terminal.toggle_panel").to_string(),
                                    ctx.tooltip.clone(),
                                    {
                                        let handle = handle_panel.clone();
                                        move |_w, cx| {
                                            handle.update(cx, |app, cx| {
                                                app.toggle_right_panel(active_tab_id, cx);
                                            });
                                        }
                                    },
                                )),
                        )
                    })
                    .into_any_element()
            } else {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(text_muted())).child("Terminal"))
                    .into_any_element()
            }
        }
        Some(TabKind::Sftp) => {
            // Full-screen dual-panel SFTP file browser tab.
            //
            // The SftpTabView owns its own TerminalView entities (one per
            // remote panel). Host switching happens in-place — no new tabs.
            sftp_view.update(cx, |view, cx| {
                view.set_state(
                    context_menu.clone(),
                    alert_controller.clone(),
                    ctx.tooltip.clone(),
                    ctx.transfer_history.clone(),
                    hosts.to_vec(),
                    handle.clone(),
                    window,
                    cx,
                );
            });
            sftp_view.clone().into_any_element()
        }
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(div().text_color(rgb(text_muted())).child("No tab"))
            .into_any_element(),
    };

    let is_terminal = active_tab
        .map(|t| t.kind == TabKind::Terminal)
        .unwrap_or(false);
    let is_sftp_tab = active_tab.map(|t| t.kind == TabKind::Sftp).unwrap_or(false);

    // Read monitor status & metrics from the active TerminalView's backend.
    // For terminal tabs this comes from `terminal_views`; SFTP tabs manage
    // their own terminal entities internally so there's no single terminal
    // entity to read here.
    let sftp_term = if is_terminal {
        terminal_entity(is_terminal, active_tab, terminal_views)
    } else {
        None
    };
    let (status, metrics, sftp_progress) = match &sftp_term {
        Some(entity) => entity.read_with(cx, |view, _cx| {
            let (status, metrics) = if let Some(m) = view.monitor() {
                (m.status(), m.metrics())
            } else {
                (RemoteStatus::Local, RemoteMetrics::default())
            };
            // Clone the live SFTP progress snapshot so the toolbar can
            // render it without holding the entity lock across the
            // render call. `None` when no transfer is in flight.
            (status, metrics, view.sftp_progress().cloned())
        }),
        None => (
            RemoteStatus::Local,
            RemoteMetrics::default(),
            // SFTP tab: read progress from whichever of the tab's two
            // panels has an active transfer. This keeps the shared
            // toolbar's animation alive across terminal ↔ SFTP switches.
            if is_sftp_tab {
                sftp_view.read_with(cx, |v, _cx| v.sftp_progress(cx))
            } else {
                None
            },
        ),
    };

    // Only expand the right-hand panel once the session is fully
    // connected. Local PTY backends report `RemoteStatus::Local` and are
    // considered ready immediately; remote (SSH / Telnet) backends cycle
    // through `Connecting` -> `Connected` (or `Disconnected`), and the
    // panel stays collapsed until they actually reach `Connected`.
    //
    // The user can still toggle the panel closed via the toolbar button even
    // when the session is ready — `panel_open` (defaulting to true) is
    // AND-ed with the readiness check so the panel never appears mid-connect.
    let panel_show = is_terminal
        && panel_open
        && matches!(status, RemoteStatus::Connected | RemoteStatus::Local);

    // Read SFTP state from the active TerminalView's backend and push it
    // into the shared SftpPanel entity.
    let (sftp_entries, sftp_cwd): (
        std::sync::Arc<Vec<crabport_sftp::FileEntry>>,
        Option<std::sync::Arc<String>>,
    ) = match &sftp_term {
        Some(entity) => entity.read_with(cx, |view, _cx| {
            if view.allow_sftp() {
                (view.sftp_entries().unwrap_or_default(), view.sftp_cwd())
            } else {
                (std::sync::Arc::new(Vec::new()), None)
            }
        }),
        None => (std::sync::Arc::new(Vec::new()), None),
    };

    // ---- SFTP callbacks ----
    //
    // Each callback clones the active terminal entity and forwards a call to
    // the matching `view.sftp_*` method. The callbacks are `None` when the
    // active tab isn't a terminal or has no terminal entity.

    let sftp_navigate: Option<Rc<dyn Fn(String, &mut App)>> = sftp_term.clone().map(|entity| {
        Rc::new(move |path: String, cx: &mut App| {
            entity.read_with(cx, |view, _cx| {
                view.sftp_navigate(&path);
            });
        }) as Rc<dyn Fn(String, &mut App)>
    });

    let sftp_download: Option<Rc<dyn Fn(String, String, &mut App)>> =
        sftp_term.clone().map(|entity| {
            Rc::new(
                move |remote_path: String, local_path: String, cx: &mut App| {
                    entity.read_with(cx, |view, _cx| {
                        view.sftp_download(&remote_path, &local_path);
                    });
                },
            ) as Rc<dyn Fn(String, String, &mut App)>
        });

    let sftp_upload: Option<Rc<dyn Fn(String, String, &mut App)>> =
        sftp_term.clone().map(|entity| {
            Rc::new(
                move |local_path: String, remote_path: String, cx: &mut App| {
                    entity.read_with(cx, |view, _cx| {
                        view.sftp_upload(&local_path, &remote_path);
                    });
                },
            ) as Rc<dyn Fn(String, String, &mut App)>
        });

    let sftp_delete: Option<Rc<dyn Fn(String, &mut App)>> = sftp_term.clone().map(|entity| {
        Rc::new(move |remote_path: String, cx: &mut App| {
            entity.read_with(cx, |view, _cx| {
                view.sftp_delete(&remote_path);
            });
        }) as Rc<dyn Fn(String, &mut App)>
    });

    let sftp_rename: Option<Rc<dyn Fn(String, String, &mut App)>> =
        sftp_term.clone().map(|entity| {
            Rc::new(move |old_path: String, new_path: String, cx: &mut App| {
                entity.read_with(cx, |view, _cx| {
                    view.sftp_rename(&old_path, &new_path);
                });
            }) as Rc<dyn Fn(String, String, &mut App)>
        });

    let sftp_edit: Option<Rc<dyn Fn(String, &mut App)>> = sftp_term.clone().map(|entity| {
        Rc::new(move |remote_path: String, cx: &mut App| {
            entity.read_with(cx, |view, _cx| {
                view.sftp_open_in_editor(&remote_path);
            });
        }) as Rc<dyn Fn(String, &mut App)>
    });

    // ---- Panel capability flags ----
    //
    // Each right-hand panel pane is shown only when the active terminal's
    // backend reports the matching capability. This replaces the old
    // `has_sftp || is_remote` gate so a Telnet tab shows History + Snippets
    // (no SFTP / Tunnels) while an SSH tab shows all four, and a local PTY
    // tab shows History + Snippets.
    // `is_remote` is retained for the tunnels-panel comment context but no
    // longer gates panel visibility — `cap_tunnels` (from the backend's
    // `allow_tunnels()`) is the source of truth now.
    let _is_remote = active_tab.map(|t| t.is_remote).unwrap_or(false);
    let (cap_sftp, cap_history, cap_snippets, cap_tunnels) = sftp_term
        .as_ref()
        .map(|entity| {
            entity.read_with(cx, |view, _cx| {
                (
                    view.allow_sftp(),
                    view.allow_history(),
                    view.allow_snippets(),
                    view.allow_tunnels(),
                )
            })
        })
        .unwrap_or((false, false, false, false));
    // SFTP panel visibility follows the backend's capability (`cap_sftp`),
    // used directly below — no separate `has_sftp` alias needed.
    sftp_panel.update(cx, |panel, cx| {
        panel.set_state(
            sftp_entries,
            sftp_cwd,
            sftp_navigate,
            sftp_download,
            sftp_upload,
            sftp_delete,
            sftp_rename,
            sftp_edit,
            active_tab_id,
            context_menu.clone(),
            alert_controller.clone(),
            ctx.tooltip.clone(),
            window,
            cx,
        );
    });

    // ---- Tunnels panel ----
    //
    // Wire the tunnel list + start/stop callbacks. Start routes to
    // `app.start_tunnel_borrowed(tunnel_id, tab_id, cx)` so the tunnel
    // reuses the active tab's SSH connection. Stop routes to
    // `app.stop_tunnel`. Only wire `on_start` for backends that can lend
    // their connection (`cap_tunnels`) — a local PTY or Telnet backend
    // exposes no tunnel source, so borrowed tunnels can't start.
    let tunnels_on_start: Option<Rc<dyn Fn(i64, &mut App)>> = if cap_tunnels {
        let handle_for_start = handle.clone();
        let tab_id = active_tab_id;
        Some(Rc::new(move |tunnel_id: i64, cx: &mut App| {
            handle_for_start.update(cx, |app, cx| {
                app.start_tunnel_borrowed(tunnel_id, tab_id, cx);
            });
        }) as Rc<dyn Fn(i64, &mut App)>)
    } else {
        None
    };
    let tunnels_on_stop: Option<Rc<dyn Fn(i64, &mut App)>> = if sftp_term.is_some() {
        let handle_for_stop = handle.clone();
        Some(Rc::new(move |tunnel_id: i64, cx: &mut App| {
            handle_for_stop.update(cx, |app, cx| {
                app.stop_tunnel(tunnel_id, cx);
            });
        }) as Rc<dyn Fn(i64, &mut App)>)
    } else {
        None
    };
    tunnels_panel.update(cx, |panel, cx| {
        panel.set_state(
            tunnel_list_for_panel,
            tunnels_on_start,
            tunnels_on_stop,
            context_menu.clone(),
            active_tab_id,
            window,
            cx,
        );
    });

    // ---- History-command panel ----
    //
    // Read the active terminal's command history + wire a paste callback
    // that writes a selected command back into the terminal's input line
    // (via `write_raw`, which bypasses history capture so the pasted
    // command isn't re-recorded).
    //
    // `on_refresh` asks the backend to re-read the TTY history file
    // (~/.bash_history / ~/.zsh_history) and broadcast a
    // `HistoryLoaded` event, which the session forwards into
    // `command_history` and the panel picks up on the next render.
    let (history_commands, history_on_paste, history_on_refresh): (
        std::sync::Arc<Vec<crate::views::panel::history_command_panel::HistoryCommand>>,
        Option<Rc<dyn Fn(String, &mut App)>>,
        Option<Rc<dyn Fn(&mut App)>>,
    ) = match &sftp_term {
        Some(entity) => {
            let cmds = entity.read_with(cx, |view, _cx| {
                view.command_history()
                    .into_iter()
                    .map(
                        |c| crate::views::panel::history_command_panel::HistoryCommand {
                            command: c,
                            timestamp: None,
                        },
                    )
                    .collect::<Vec<_>>()
            });
            let cmds = std::sync::Arc::new(cmds);
            let term_for_paste = entity.clone();
            let on_paste: Rc<dyn Fn(String, &mut App)> =
                Rc::new(move |cmd: String, cx: &mut App| {
                    term_for_paste.read_with(cx, |view, _cx| {
                        view.write_raw(cmd.as_bytes());
                    });
                });
            let term_for_refresh = entity.clone();
            let on_refresh: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
                term_for_refresh.read_with(cx, |view, _cx| {
                    view.refresh_history();
                });
            });
            (cmds, Some(on_paste), Some(on_refresh))
        }
        None => (std::sync::Arc::new(Vec::new()), None, None),
    };
    history_panel.update(cx, |panel, cx| {
        panel.set_state(
            history_commands,
            history_on_paste,
            history_on_refresh,
            ctx.notifications.clone(),
            ctx.tooltip.clone(),
            window,
            cx,
        );
    });

    // ---- Snippets panel ----
    //
    // Snippets are global (Store-backed), so we only need to wire the
    // run + paste callbacks to the active terminal. The panel reloads
    // its list from the Store inside `set_state`.
    let (snippets_on_run, snippets_on_paste): (
        Option<Rc<dyn Fn(String, &mut App)>>,
        Option<Rc<dyn Fn(String, &mut App)>>,
    ) = match &sftp_term {
        Some(entity) => {
            let term_for_run = entity.clone();
            let on_run: Rc<dyn Fn(String, &mut App)> = Rc::new(move |cmd: String, cx: &mut App| {
                term_for_run.read_with(cx, |view, _cx| {
                    view.write_raw(format!("{}\r", cmd).as_bytes());
                });
            });
            let term_for_paste = entity.clone();
            let on_paste: Rc<dyn Fn(String, &mut App)> =
                Rc::new(move |cmd: String, cx: &mut App| {
                    term_for_paste.read_with(cx, |view, _cx| {
                        view.write_raw(cmd.as_bytes());
                    });
                });
            (Some(on_run), Some(on_paste))
        }
        None => (None, None),
    };
    snippets_panel.update(cx, |panel, cx| {
        panel.set_state(
            snippets_on_run,
            snippets_on_paste,
            ctx.tooltip.clone(),
            window,
            cx,
        );
    });

    // ---- Host-key prompt ----
    //
    // If the active terminal view has a pending host-key prompt (pushed by
    // the SSH backend's `check_server_key` via the verifier), surface it via
    // the global `AlertController`. We only trigger when the controller is
    // idle so we don't re-spawn the dialog on every render while it's
    // already showing — the overlay retains the `PendingHostKey` until the
    // user resolves it (the alert's confirm/cancel callbacks call
    // `TerminalView::resolve_pending_host_key`).
    if let Some(terminal_entity) = &sftp_term {
        let pending = terminal_entity.read_with(cx, |view, _| view.pending_host_key_info());
        if let Some(info) = pending {
            let controller_busy = alert_controller.read_with(cx, |c, _| c.is_active());
            if !controller_busy {
                let term_for_confirm = terminal_entity.clone();
                let on_confirm = Rc::new(move |_w: &mut Window, cx: &mut App| {
                    term_for_confirm.update(cx, |view, _cx| {
                        view.resolve_pending_host_key(true);
                    });
                });
                let term_for_cancel = terminal_entity.clone();
                let on_cancel = Rc::new(move |_w: &mut Window, cx: &mut App| {
                    term_for_cancel.update(cx, |view, _cx| {
                        view.resolve_pending_host_key(false);
                    });
                });
                alert_controller.update(cx, |c, cx| {
                    c.show(
                        AlertState {
                            severity: AlertSeverity::Warning,
                            title: t!("terminal.host_key_unknown").to_string().into(),
                            description: {
                                let host_port = if info.port == 22 {
                                    info.host.clone()
                                } else {
                                    format!("{}:{}", info.host, info.port)
                                };
                                Some(
                                    t!("terminal.host_key_prompt", host = host_port.as_str())
                                        .to_string()
                                        .into(),
                                )
                            },
                            details: vec![
                                (
                                    t!("terminal.host_key_algo").to_string().into(),
                                    info.algo.clone().into(),
                                ),
                                (
                                    t!("terminal.host_key_fingerprint").to_string().into(),
                                    info.fingerprint.clone().into(),
                                ),
                            ],
                            confirm_label: t!("terminal.host_key_accept").to_string().into(),
                            cancel_label: t!("terminal.host_key_cancel").to_string().into(),
                            open: true,
                            on_confirm: Some(on_confirm),
                            on_cancel: Some(on_cancel),
                        },
                        cx,
                    );
                });
            }
        }
    }

    div()
        .flex_1()
        .h_full()
        .bg(rgb(bg_base()))
        .flex()
        .flex_col()
        .child(render_tab_bar(
            handle,
            tabs,
            active_tab_id,
            active_tab.map(|t| t.kind == TabKind::Home).unwrap_or(false),
            on_close,
        ))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_row()
                .overflow_hidden()
                .relative()
                .child(view)
                .when(
                    panel_show && (cap_sftp || cap_history || cap_snippets || cap_tunnels),
                    |el| {
                        // Resize divider handle between terminal and panel.
                        // A narrow strip with negative margins so it overlaps
                        // the panel border while remaining grabbable.
                        let handle_for_resize = handle.clone();
                        el.child(
                            div()
                                .id("panel-resize-handle")
                                .flex_shrink_0()
                                .h_full()
                                .w(px(crate::layouts::panel::PANEL_DIVIDER_HIT * 2.0))
                                .ml(px(-crate::layouts::panel::PANEL_DIVIDER_HIT))
                                .mr(px(-crate::layouts::panel::PANEL_DIVIDER_HIT))
                                .cursor_col_resize()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, {
                                    let handle = handle_for_resize.clone();
                                    let pw = panel_width;
                                    move |event, _window, cx| {
                                        handle.update(cx, |app, cx| {
                                            app.panel_drag =
                                                Some(crate::layouts::panel::PanelDrag {
                                                    start_width: pw,
                                                    start_x: f32::from(event.position.x),
                                                    width: pw,
                                                });
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                    },
                )
                .child({
                    let handle_for_panel = handle.clone();
                    // Capture the capability flags so the `on_change` closure
                    // can recompute the visible-pane-kind list and map the
                    // positional index back to a `PanelKind`. The flags are a
                    // snapshot from this render — if the active tab changes
                    // the next render rebuilds the closure with fresh flags.
                    let c_sftp = cap_sftp;
                    let c_history = cap_history;
                    let c_snippets = cap_snippets;
                    let c_tunnels = cap_tunnels;
                    render_panel(
                        panel_show,
                        panel_active_tab,
                        PanelCaps {
                            sftp: c_sftp,
                            history: c_history,
                            snippets: c_snippets,
                            tunnels: c_tunnels,
                        },
                        sftp_panel.clone(),
                        snippets_panel.clone(),
                        history_panel.clone(),
                        tunnels_panel.clone(),
                        Some(std::rc::Rc::new(move |idx, _w, cx| {
                            // Rebuild the visible-kind list in the same fixed
                            // order as `render_panel` so the index aligns.
                            let mut kinds: Vec<PanelKind> = Vec::with_capacity(4);
                            if c_sftp {
                                kinds.push(PanelKind::Sftp);
                            }
                            if c_history {
                                kinds.push(PanelKind::History);
                            }
                            if c_snippets {
                                kinds.push(PanelKind::Snippets);
                            }
                            if c_tunnels {
                                kinds.push(PanelKind::Tunnels);
                            }
                            handle_for_panel.update(cx, |app, cx| {
                                if let Some(k) = kinds.get(idx).copied() {
                                    // Store per-tab so each terminal
                                    // connection keeps its own panel choice.
                                    app.panel_active_tab.insert(active_tab_id, k);
                                    cx.notify();
                                }
                            });
                        })),
                        panel_width,
                        panel_dragging,
                    )
                })
                // Transparent canvas whose paint callback registers
                // window-level mouse listeners for the panel resize drag.
                // `window.on_mouse_event` can only be called during paint,
                // so we need this canvas to hook into the paint phase. The
                // listeners are registered every frame but no-op when
                // `panel_drag` is `None`.
                .child({
                    let handle_for_canvas = handle.clone();
                    canvas(
                        |_bounds, _window, _cx| {},
                        move |_bounds, _state, window, _cx| {
                            let handle_for_move = handle_for_canvas.clone();
                            window.on_mouse_event({
                                let handle = handle_for_move.clone();
                                move |event: &MouseMoveEvent, phase, window, cx| {
                                    if phase != DispatchPhase::Capture {
                                        return;
                                    }
                                    let _ = handle.update(cx, |app, cx| {
                                        if let Some(ref mut drag) = app.panel_drag {
                                            let delta = drag.start_x - f32::from(event.position.x);
                                            let eff_max =
                                                crate::layouts::panel::effective_max_panel_width(
                                                    f32::from(window.viewport_size().width),
                                                );
                                            let new_width = (drag.start_width + delta).clamp(
                                                crate::layouts::panel::MIN_PANEL_WIDTH,
                                                eff_max,
                                            );
                                            if (new_width - drag.width).abs() > 0.01 {
                                                drag.width = new_width;
                                                cx.notify();
                                            }
                                        }
                                    });
                                }
                            });
                            window.on_mouse_event({
                                let handle = handle_for_move.clone();
                                move |_event: &MouseUpEvent, phase, window, cx| {
                                    if phase != DispatchPhase::Capture {
                                        return;
                                    }
                                    let _ = handle.update(cx, |app, cx| {
                                        if let Some(drag) = app.panel_drag.take() {
                                            let eff_max =
                                                crate::layouts::panel::effective_max_panel_width(
                                                    f32::from(window.viewport_size().width),
                                                );
                                            let _ = crabport_core::config::update(|cfg| {
                                                cfg.appearance.panel_width = drag.width.clamp(
                                                    crate::layouts::panel::MIN_PANEL_WIDTH,
                                                    eff_max,
                                                );
                                            });
                                            cx.notify();
                                        }
                                    });
                                }
                            });
                        },
                    )
                    .w_0()
                    .h_0()
                }),
        )
        .child(render_terminal_toolbar(
            TerminalToolbarInput::new(is_terminal, status, metrics, sftp_progress),
            // The caller already passes `app_ctx` (which owns the context
            // menu controller) into `render_content`, so we grab it
            // directly here. Going through `handle.read_with(cx, ...)` would
            // panic — `CrabportApp` is already borrowed mutably by its own
            // `render` method, and GPUI forbids nested reads of the same
            // entity ("cannot read while it is already being updated").
            Some(ctx.context_menu.clone()),
            // SFTP tab: inject the "transfer history" toggle button as a
            // trailing element so it appears in the toolbar without the
            // terminal toolbar knowing about SFTP. Terminal tabs pass an
            // empty vec (no trailing buttons).
            if is_sftp_tab {
                let transfer_history = ctx.transfer_history.clone();
                let on = transfer_history.read_with(cx, |c, _| c.is_open());
                vec![
                    crate::views::sftp::render_sftp_history_toggle(transfer_history, on)
                        .into_any_element(),
                ]
            } else {
                Vec::new()
            },
            {
                let handle = handle.clone();
                move |id, cx| {
                    // Toggle the slot's visibility flag in config and persist.
                    // The toolbar reads the flag on the next render, so the
                    // ctxmenu checkmark updates immediately.
                    let _ = crabport_core::config::update(|cfg| {
                        cfg.appearance.terminal.toolbar.toggle(id);
                    });
                    let _ = handle.update(cx, |_, cx| cx.notify());
                }
            },
        ))
}

// ---------------------------------------------------------------------------
// Terminal split-pane rendering
// ---------------------------------------------------------------------------
//
// Renders a [`SplitNode`] tree as nested flex containers. Each split draws a
// draggable divider between its two children. Clicking a pane focuses it
// (updates the tab's active pane so the toolbar follows).

/// Recursively render a split node into a full-size element using
/// gpui-component's `ResizablePanelGroup` for drag-to-resize dividers.
fn render_split_node(
    node: &SplitNode,
    active_pane: u64,
    tab_id: u64,
    pane_views: &HashMap<u64, Entity<TerminalView>>,
    handle: &Entity<CrabportApp>,
) -> AnyElement {
    use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable};
    match node {
        SplitNode::Pane(pane_id) => {
            render_pane(*pane_id, active_pane, tab_id, pane_views, handle).into_any_element()
        }
        SplitNode::Split { dir, a, b, .. } => {
            let dir = *dir;
            let group_id =
                ElementId::Name(format!("split-group-{}-{}", tab_id, leaf_pane_id(node)).into());
            let axis = match dir {
                SplitDir::Vertical => gpui::Axis::Horizontal,
                SplitDir::Horizontal => gpui::Axis::Vertical,
            };
            // Render children. For a nested split child, wrap it in a
            // `resizable_panel` so the inner group fills its allocated space.
            let child_a = render_split_child(a, active_pane, tab_id, pane_views, handle);
            let child_b = render_split_child(b, active_pane, tab_id, pane_views, handle);

            let group = if axis == gpui::Axis::Horizontal {
                h_resizable(group_id)
            } else {
                v_resizable(group_id)
            };
            group
                .child(resizable_panel().child(child_a))
                .child(resizable_panel().child(child_b))
                .into_any_element()
        }
    }
}

/// Render a split child — either a leaf pane or a nested group (wrapped
/// in a `resizable_panel` by the caller's group).
fn render_split_child(
    node: &SplitNode,
    active_pane: u64,
    tab_id: u64,
    pane_views: &HashMap<u64, Entity<TerminalView>>,
    handle: &Entity<CrabportApp>,
) -> AnyElement {
    match node {
        SplitNode::Pane(pane_id) => {
            render_pane(*pane_id, active_pane, tab_id, pane_views, handle).into_any_element()
        }
        SplitNode::Split { .. } => {
            // Nested split: render as a group, which itself becomes the child.
            render_split_node(node, active_pane, tab_id, pane_views, handle)
        }
    }
}

/// Render a single terminal pane with click-to-focus + active highlight.
fn render_pane(
    pane_id: u64,
    active_pane: u64,
    tab_id: u64,
    pane_views: &HashMap<u64, Entity<TerminalView>>,
    handle: &Entity<CrabportApp>,
) -> impl IntoElement {
    let is_active = pane_id == active_pane;
    let view = pane_views.get(&pane_id).cloned();
    let view_for_focus = view.clone();
    let mut el = div()
        .id(ElementId::Name(
            format!("pane-{}-{}", tab_id, pane_id).into(),
        ))
        .size_full()
        // Occlude so this pane's hitbox blocks sibling panes' mouse
        // handlers — without this, overlapping hitboxes (e.g. when the
        // resizable-panel flex layout doesn't fully clip children) cause a
        // single click to fire `on_mouse_down` on *every* pane, re-focusing
        // the wrong one.
        .occlude()
        .when(is_active, |el| el.bg(rgba((surface_hover() << 8) | 0x18)))
        .on_mouse_down(MouseButton::Left, {
            let handle = handle.clone();
            move |_e, w, cx| {
                handle.update(cx, |app, cx| {
                    app.focus_pane(tab_id, pane_id, cx);
                });
                if let Some(view) = &view_for_focus {
                    let fh = view.read_with(cx, |v, cx| v.focus_handle(cx));
                    w.focus(&fh);
                }
                cx.stop_propagation();
            }
        });
    if let Some(view) = view {
        el = el.child(view);
    }
    el
}

/// The pane id of the first leaf under a node (used to key dividers).
fn leaf_pane_id(node: &SplitNode) -> u64 {
    match node {
        SplitNode::Pane(id) => *id,
        SplitNode::Split { a, .. } => leaf_pane_id(a),
    }
}

/// A compact icon button for the split-action overlay (top-right of the
/// terminal content area). Ghost style: transparent bg, eased hover bg.
/// Uses the global [`TooltipController`] for hover tooltips with fade-in/out.
fn render_split_button(
    id: &'static str,
    icon: &'static str,
    tooltip_text: String,
    tooltip_ctrl: Entity<crate::components::tooltip::TooltipController>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let btn_id = ElementId::Name(format!("{}-btn", id).into());
    let hover_bg = rgba((surface_hover() << 8) | 0xFF);
    let rest_bg = rgba((surface_hover() << 8) | 0x00);
    let tooltip_text_clone = tooltip_text.clone();
    div()
        .id(btn_id.clone())
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded(RADIUS_SM)
        // Pre-set the rest (transparent) bg so the transition registry has
        // a concrete `Some(bg)` to interpolate *from* on hover-in.
        .bg(rest_bg)
        // `on_hover` / `on_click` must be on the AnimatedWrapper (i.e. after
        // `with_transition`), not on the raw div — the wrapper's own render
        // also calls `on_hover` internally and panics if one is already set.
        .with_transition(btn_id)
        .on_hover(move |hovered, w, cx| {
            if *hovered {
                tooltip_ctrl.update(cx, |t, cx| {
                    t.show(tooltip_text_clone.clone(), w.mouse_position(), cx);
                });
            } else {
                tooltip_ctrl.update(cx, |t, cx| {
                    t.hide(cx);
                });
            }
        })
        .on_click(move |_e, w, cx| {
            on_click(w, cx);
            cx.stop_propagation();
        })
        .transition_on_hover(DURATION_FAST, EASE_STANDARD, move |hovered, el| {
            if *hovered {
                el.bg(hover_bg)
            } else {
                el.bg(rgba((surface_hover() << 8) | 0x00))
            }
        })
        .child(
            svg()
                .path(icon)
                .size(px(14.0))
                .text_color(rgb(text_muted())),
        )
}
