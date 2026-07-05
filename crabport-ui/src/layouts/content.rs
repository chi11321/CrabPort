use std::collections::HashMap;
use std::rc::Rc;

use gpui::*;
use rust_i18n::t;

use crate::app::{CrabportApp, SidebarItem, Tab, TabKind};
use crate::color::*;
use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::layouts::panel::render_panel;
use crate::layouts::tabbar::render_tab_bar;
use crate::layouts::terminal_toolbar::render_terminal_toolbar;
use crate::views::hosts::{ConnectionFormState, ConnectionHost, HostsView};
use crate::views::panel::sftp::SftpPanel;
use crate::views::terminal::TerminalView;

pub fn render_content(
    selected: SidebarItem,
    handle: &Entity<CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    terminal_views: &HashMap<u64, Entity<TerminalView>>,
    hosts: &[ConnectionHost],
    form_entity: Option<&ConnectionFormState>,
    sftp_panel: &Entity<SftpPanel>,
    snippets_panel: &Entity<crate::views::panel::snippets_panel::SnippetsPanel>,
    history_panel: &Entity<crate::views::panel::history_command_panel::HistoryCommandPanel>,
    tunnels_panel: &Entity<crate::views::panel::tunnels_panel::TunnelsPanel>,
    // Active index of the right-hand panel tab strip (SFTP / History /
    // Snippets). Read by the caller (which owns the `CrabportApp` borrow)
    // and passed in to avoid a nested `handle.read_with` during render.
    panel_active_tab: usize,
    hosts_view: &Entity<HostsView>,
    snippets_view: &Entity<crate::views::snippets::SnippetsView>,
    tunnels_view: &Entity<crate::views::tunnels::TunnelsView>,
    // Pre-read by the caller (which owns the `CrabportApp` borrow) to avoid
    // a nested `handle.read_with` during render — same reason as
    // `panel_active_tab`.
    tunnel_list: Vec<crate::views::tunnels::TunnelView>,
    tunnel_form_state: Option<crate::views::tunnels::TunnelFormState>,
    alert_controller: &Entity<AlertController>,
    context_menu: &Entity<ContextMenuController>,
    // Pre-read by the caller (which owns the `CrabportApp` borrow) to avoid
    // a nested `handle.read_with` during render — same reason as
    // `tunnel_list` / `panel_active_tab`. Used to fire toast notifications
    // from views that don't route their actions through `CrabportApp`
    // methods (e.g. `SnippetsView::save_edit`).
    notification_controller: &Entity<crate::components::notification::NotificationController>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let active_tab = tabs.iter().find(|t| t.id == active_tab_id);
    // Clone the tunnel list for the panel — the full-page TunnelsView
    // (SidebarItem::Tunnels arm below) consumes the original `tunnel_list`.
    let tunnel_list_for_panel = tunnel_list.clone();
    let handle_c = handle.clone();
    let on_close: Rc<dyn Fn(u64, &mut Window, &mut App)> = Rc::new(move |id, _w, cx| {
        handle_c.update(cx, |app, cx| {
            app.close_tab(id, cx);
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
                    let on_edit_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_edit);
                    let on_remove_rc: Rc<dyn Fn(i64, &mut Window, &mut App)> = Rc::new(on_remove);

                    hosts_view.update(cx, |view, cx| {
                        view.set_state(
                            hosts.to_vec(),
                            form_entity.cloned(),
                            Some(on_new_rc),
                            Some(on_connect_rc),
                            Some(on_edit_rc),
                            Some(on_remove_rc),
                            context_menu.clone(),
                            alert_controller.clone(),
                            cx,
                        );
                    });

                    hosts_view.clone().into_any_element()
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
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    snippets_view.update(cx, |view, cx| {
                        view.set_state(
                            rows,
                            context_menu.clone(),
                            alert_controller.clone(),
                            notification_controller.clone(),
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
                            .text_color(rgb(TEXT_MUTED))
                            .child(selected.label().to_string()),
                    )
                    .into_any_element(),
            }
        }
        Some(TabKind::Terminal) => {
            if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
                div()
                    .size_full()
                    .pt_2()
                    .pl_2()
                    .key_context("Terminal")
                    .child(terminal_entity.clone())
                    .into_any_element()
            } else {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_color(rgb(TEXT_MUTED)).child("Terminal"))
                    .into_any_element()
            }
        }
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(div().text_color(rgb(TEXT_MUTED)).child("No tab"))
            .into_any_element(),
    };

    let is_terminal = active_tab
        .map(|t| t.kind == TabKind::Terminal)
        .unwrap_or(false);

    // Read monitor status & metrics from the active TerminalView's backend
    let (status, metrics, sftp_progress) = if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
            terminal_entity.read_with(cx, |view, _cx| {
                let (status, metrics) = if let Some(m) = view.monitor() {
                    (m.status(), m.metrics())
                } else {
                    (
                        crabport_terminal::terminal::RemoteStatus::Local,
                        crabport_terminal::terminal::RemoteMetrics::default(),
                    )
                };
                // Clone the live SFTP progress snapshot so the toolbar can
                // render it without holding the entity lock across the
                // render call. `None` when no transfer is in flight.
                (status, metrics, view.sftp_progress().cloned())
            })
        } else {
            (
                crabport_terminal::terminal::RemoteStatus::Local,
                crabport_terminal::terminal::RemoteMetrics::default(),
                None,
            )
        }
    } else {
        (
            crabport_terminal::terminal::RemoteStatus::Local,
            crabport_terminal::terminal::RemoteMetrics::default(),
            None,
        )
    };

    // Read SFTP state from the active TerminalView's backend and push it
    // into the shared SftpPanel entity.
    let (sftp_entries, sftp_cwd): (
        std::sync::Arc<Vec<(String, bool)>>,
        Option<std::sync::Arc<String>>,
    ) = if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
            terminal_entity.read_with(cx, |view, _cx| {
                if view.allow_sftp() {
                    (view.sftp_entries().unwrap_or_default(), view.sftp_cwd())
                } else {
                    (std::sync::Arc::new(Vec::new()), None)
                }
            })
        } else {
            (std::sync::Arc::new(Vec::new()), None)
        }
    } else {
        (std::sync::Arc::new(Vec::new()), None)
    };

    // Build SFTP navigate callback
    let sftp_navigate: Option<Rc<dyn Fn(String, &mut App)>> = if is_terminal {
        active_tab.and_then(|tab| {
            terminal_views.get(&tab.id).map(|entity| {
                let entity = entity.clone();
                Rc::new(move |path: String, cx: &mut App| {
                    entity.read_with(cx, |view, _cx| {
                        view.sftp_navigate(&path);
                    });
                }) as Rc<dyn Fn(String, &mut App)>
            })
        })
    } else {
        None
    };

    // Build SFTP download callback. Mirrors `sftp_navigate`'s shape: a thin
    // closure that forwards `(remote_path, local_path)` to the active
    // terminal's backend. The backend reports completion asynchronously via
    // `BackendEvent::SftpTransferFinished`, which `TerminalView` already
    // surfaces as a status line — no extra plumbing needed here.
    let sftp_download: Option<Rc<dyn Fn(String, String, &mut App)>> = if is_terminal {
        active_tab.and_then(|tab| {
            terminal_views.get(&tab.id).map(|entity| {
                let entity = entity.clone();
                Rc::new(
                    move |remote_path: String, local_path: String, cx: &mut App| {
                        entity.read_with(cx, |view, _cx| {
                            view.sftp_download(&remote_path, &local_path);
                        });
                    },
                ) as Rc<dyn Fn(String, String, &mut App)>
            })
        })
    } else {
        None
    };

    // Build SFTP upload callback. Same shape as `sftp_download` but with the
    // argument order swapped to match `view.sftp_upload(local, remote)`.
    let sftp_upload: Option<Rc<dyn Fn(String, String, &mut App)>> = if is_terminal {
        active_tab.and_then(|tab| {
            terminal_views.get(&tab.id).map(|entity| {
                let entity = entity.clone();
                Rc::new(
                    move |local_path: String, remote_path: String, cx: &mut App| {
                        entity.read_with(cx, |view, _cx| {
                            view.sftp_upload(&local_path, &remote_path);
                        });
                    },
                ) as Rc<dyn Fn(String, String, &mut App)>
            })
        })
    } else {
        None
    };

    // Build SFTP delete callback. Forwards the remote path to the backend's
    // `sftp_delete`, which stats the path to choose `remove_file` vs
    // recursive `remove_dir`.
    let sftp_delete: Option<Rc<dyn Fn(String, &mut App)>> = if is_terminal {
        active_tab.and_then(|tab| {
            terminal_views.get(&tab.id).map(|entity| {
                let entity = entity.clone();
                Rc::new(move |remote_path: String, cx: &mut App| {
                    entity.read_with(cx, |view, _cx| {
                        view.sftp_delete(&remote_path);
                    });
                }) as Rc<dyn Fn(String, &mut App)>
            })
        })
    } else {
        None
    };

    let has_sftp = !sftp_entries.is_empty();
    let is_remote = active_tab.map(|t| t.is_remote).unwrap_or(false);
    sftp_panel.update(cx, |panel, cx| {
        panel.set_state(
            sftp_entries,
            sftp_cwd,
            sftp_navigate,
            sftp_download,
            sftp_upload,
            sftp_delete,
            active_tab_id,
            context_menu.clone(),
            alert_controller.clone(),
            window,
            cx,
        );
    });

    // ---- Tunnels panel ----
    //
    // Wire the tunnel list + start/stop callbacks. Start routes to
    // `app.start_tunnel_borrowed(tunnel_id, tab_id, cx)` so the tunnel
    // reuses the active tab's SSH connection. Stop routes to
    // `app.stop_tunnel`. Only wire for remote (SSH) tabs — local PTY
    // backends expose no tunnel source, so borrowed tunnels can't start.
    let tunnels_on_start: Option<Rc<dyn Fn(i64, &mut App)>> = if is_terminal && is_remote {
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
    let tunnels_on_stop: Option<Rc<dyn Fn(i64, &mut App)>> = if is_terminal {
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
    let (history_commands, history_on_paste): (
        std::sync::Arc<Vec<crate::views::panel::history_command_panel::HistoryCommand>>,
        Option<Rc<dyn Fn(String, &mut App)>>,
    ) = if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
            let cmds = terminal_entity.read_with(cx, |view, _cx| {
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
            let term_for_paste = terminal_entity.clone();
            let on_paste: Rc<dyn Fn(String, &mut App)> =
                Rc::new(move |cmd: String, cx: &mut App| {
                    term_for_paste.read_with(cx, |view, _cx| {
                        view.write_raw(cmd.as_bytes());
                    });
                });
            (cmds, Some(on_paste))
        } else {
            (std::sync::Arc::new(Vec::new()), None)
        }
    } else {
        (std::sync::Arc::new(Vec::new()), None)
    };
    history_panel.update(cx, |panel, cx| {
        panel.set_state(history_commands, history_on_paste, window, cx);
    });

    // ---- Snippets panel ----
    //
    // Snippets are global (Store-backed), so we only need to wire the
    // run + paste callbacks to the active terminal. The panel reloads
    // its list from the Store inside `set_state`.
    let (snippets_on_run, snippets_on_paste): (
        Option<Rc<dyn Fn(String, &mut App)>>,
        Option<Rc<dyn Fn(String, &mut App)>>,
    ) = if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
            let term_for_run = terminal_entity.clone();
            let on_run: Rc<dyn Fn(String, &mut App)> = Rc::new(move |cmd: String, cx: &mut App| {
                term_for_run.read_with(cx, |view, _cx| {
                    view.write_raw(format!("{}\r", cmd).as_bytes());
                });
            });
            let term_for_paste = terminal_entity.clone();
            let on_paste: Rc<dyn Fn(String, &mut App)> =
                Rc::new(move |cmd: String, cx: &mut App| {
                    term_for_paste.read_with(cx, |view, _cx| {
                        view.write_raw(cmd.as_bytes());
                    });
                });
            (Some(on_run), Some(on_paste))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };
    snippets_panel.update(cx, |panel, cx| {
        panel.set_state(snippets_on_run, snippets_on_paste, window, cx);
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
    if is_terminal {
        if let Some(terminal_entity) = active_tab.and_then(|tab| terminal_views.get(&tab.id)) {
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
    }

    div()
        .flex_1()
        .h_full()
        .bg(rgb(BG_BASE))
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
                .child(view)
                .child({
                    let handle_for_panel = handle.clone();
                    render_panel(
                        is_terminal,
                        panel_active_tab,
                        has_sftp || is_remote,
                        sftp_panel.clone(),
                        snippets_panel.clone(),
                        history_panel.clone(),
                        tunnels_panel.clone(),
                        Some(std::rc::Rc::new(move |idx, _w, cx| {
                            handle_for_panel.update(cx, |app, cx| {
                                app.panel_active_tab = idx;
                                cx.notify();
                            });
                        })),
                    )
                }),
        )
        .child(render_terminal_toolbar(
            is_terminal,
            status,
            metrics,
            sftp_progress,
        ))
}
