//! Tunnel lifecycle management — form dialogs, start/stop (owned + borrowed), and removal.

use std::sync::Arc;

use gpui::*;
use rust_i18n::t;

use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};
use crabport_core::credential::{CredentialKind as CoreCredentialKind, TunnelEntry, TunnelKind};
use crabport_ssh::session::SshConnectionInfo;
use crabport_ssh::{CrabPortTunnel, OwnedSession, TunnelManager};

use super::*;

impl CrabportApp {
    /// Open the tunnel form dialog in create mode (blank fields).
    pub fn open_tunnel_form_for_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Lazily create the form state on first use.
        if self.tunnel_form.is_none() {
            let mut form = crate::views::tunnels::TunnelFormState::new(window, cx);
            let app = cx.entity().clone();
            form.on_close = Some(std::rc::Rc::new(move |_w, cx| {
                app.update(cx, |app, cx| app.close_tunnel_form(cx));
            }));
            let app = cx.entity().clone();
            form.on_save = Some(std::rc::Rc::new(move |out, w, cx| {
                app.update(cx, |app, cx| app.save_tunnel(out, w, cx));
            }));
            self.tunnel_form = Some(form);
        }
        if let Some(ref mut form) = self.tunnel_form {
            form.open_for_create(window, cx);
        }
        cx.notify();
    }

    /// Open the tunnel form dialog in edit mode, populated from a saved
    /// tunnel config.
    pub fn open_tunnel_form_for_edit(
        &mut self,
        tunnel_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Don't allow editing a running tunnel — the ports may be bound.
        if self.app_ctx.tunnels.is_running(tunnel_id) {
            tracing::warn!("tunnel {tunnel_id} is running; refusing to edit");
            return;
        }
        let store = AppState::store(cx);
        let entry = match store.lock().find_tunnel(tunnel_id) {
            Ok(Some(e)) => e,
            _ => return,
        };
        if self.tunnel_form.is_none() {
            let mut form = crate::views::tunnels::TunnelFormState::new(window, cx);
            let app = cx.entity().clone();
            form.on_close = Some(std::rc::Rc::new(move |_w, cx| {
                app.update(cx, |app, cx| app.close_tunnel_form(cx));
            }));
            let app = cx.entity().clone();
            form.on_save = Some(std::rc::Rc::new(move |out, w, cx| {
                app.update(cx, |app, cx| app.save_tunnel(out, w, cx));
            }));
            self.tunnel_form = Some(form);
        }
        if let Some(ref mut form) = self.tunnel_form {
            form.open_for_edit(&entry, window, cx);
        }
        cx.notify();
    }

    /// Close the tunnel form dialog. Mirrors `close_connection_form`.
    pub fn close_tunnel_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.tunnel_form {
            form.close();
        }
        // Destroy after the exit animation.
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.tunnel_form.is_some() {
                    let tabs_id = gpui::ElementId::Name("tunnel-kind-tabs".into());
                    crate::components::tabs::Tabs::cleanup_animation(&tabs_id, 3);
                    app.tunnel_form = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Persist a tunnel config (insert or update) from the form output.
    pub fn save_tunnel(
        &mut self,
        out: crate::views::tunnels::TunnelFormOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Basic validation.
        if out.name.trim().is_empty() || out.host_id == 0 || out.bind_port == 0 {
            tracing::warn!(
                "save_tunnel: validation failed — name={:?} host_id={} bind_port={}",
                out.name,
                out.host_id,
                out.bind_port
            );
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let name = out.name.clone();
        let entry = TunnelEntry {
            id: out.editing_id.unwrap_or(0),
            name: out.name,
            host_id: out.host_id,
            kind: out.kind,
            bind_addr: if out.bind_addr.is_empty() {
                "127.0.0.1".to_string()
            } else {
                out.bind_addr
            },
            bind_port: out.bind_port,
            target_host: out.target_host,
            target_port: out.target_port,
            created_at: now,
            favorite: out.favorite,
            group_id: out.group_id,
        };
        let store = AppState::store(cx);
        match out.editing_id {
            Some(_id) => {
                if let Err(e) = store.lock().update_tunnel(&entry) {
                    tracing::error!("update_tunnel failed: {e}");
                    self.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            Notification::new(t!("tunnels.notif_save_failed_title").to_string())
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!("tunnels.notif_save_failed_msg", name = name.as_str())
                                        .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                            cx,
                        );
                    });
                    cx.notify();
                    return;
                }
                self.app_ctx.tunnels.update_config(entry);
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("tunnels.notif_save_updated_title").to_string())
                            .level(NotificationLevel::Success)
                            .message(
                                t!("tunnels.notif_save_updated_msg", name = name.as_str())
                                    .to_string(),
                            )
                            .duration(std::time::Duration::from_secs(3)),
                        cx,
                    );
                });
            }
            None => {
                let id = match store.lock().add_tunnel(&entry) {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::error!("add_tunnel failed: {e}");
                        self.app_ctx.notifications.update(cx, |c, cx| {
                            c.show(
                                Notification::new(
                                    t!("tunnels.notif_save_failed_title").to_string(),
                                )
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!("tunnels.notif_save_failed_msg", name = name.as_str())
                                        .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                                cx,
                            );
                        });
                        cx.notify();
                        return;
                    }
                };
                let mut entry = entry;
                entry.id = id;
                self.app_ctx.tunnels.add(entry);
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("tunnels.notif_save_created_title").to_string())
                            .level(NotificationLevel::Success)
                            .message(
                                t!("tunnels.notif_save_created_msg", name = name.as_str())
                                    .to_string(),
                            )
                            .duration(std::time::Duration::from_secs(3)),
                        cx,
                    );
                });
            }
        }
        self.close_tunnel_form(cx);
    }

    /// Start a tunnel using a fresh, owned SSH connection (started from the
    /// Tunnels page). Resolves the host's credential + proxy, builds an
    /// `OwnedSession`, then a `TunnelManager`, and starts the tunnel.
    pub fn start_tunnel_owned(
        &mut self,
        tunnel_id: i64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.app_ctx.tunnels.is_running(tunnel_id) {
            tracing::warn!("tunnel {tunnel_id} already running");
            return;
        }
        // Resolve the tunnel config + host + credential + proxy.
        let store = AppState::store(cx);
        let entry = match store.lock().find_tunnel(tunnel_id) {
            Ok(Some(e)) => e,
            _ => return,
        };
        let tunnel_name = entry.name.clone();
        let host = match store.lock().hosts() {
            Ok(hosts) => hosts.into_iter().find(|h| h.id == entry.host_id),
            Err(_) => None,
        };
        let Some(host) = host else {
            tracing::error!("tunnel {tunnel_id}: host {} not found", entry.host_id);
            return;
        };
        let cred = host
            .credential_id
            .and_then(|cid| store.lock().find_credential(cid).ok().flatten());
        let (password, private_key, passphrase) = match cred.as_ref() {
            Some(c) if c.kind == CoreCredentialKind::Certificate => (
                String::new(),
                if c.private_key.is_empty() {
                    None
                } else {
                    Some(c.private_key.as_str())
                },
                if c.secret.is_empty() {
                    None
                } else {
                    Some(c.secret.as_str())
                },
            ),
            Some(c) => (c.secret.clone(), None, None),
            None => (String::new(), None, None),
        };
        let proxy_config = host
            .proxy_id
            .and_then(|pid| store.lock().find_proxy_config(pid).ok().flatten());

        let mut info =
            SshConnectionInfo::new(&host.host, &host.username, &password).with_port(host.port);
        if let Some(pk) = private_key {
            info = info.with_private_key(pk, passphrase.map(|s| s.to_string()));
        }
        if let Some(p) = proxy_config {
            info = info.with_proxy(p);
        }

        // The tunnel start logic touches tokio I/O (`TcpListener::bind`, the
        // SSH connect path, russh channels) so it MUST run on the shared
        // `TOKIO` runtime — GPUI's `cx.spawn` is a smol executor with no
        // tokio reactor. We `TOKIO.spawn` the work and signal completion via
        // a `tokio::sync::oneshot` (which is `Send` and uses standard
        // `std::task::Waker`s, so smol's `cx.spawn` can await its
        // `Receiver` without deadlocking — unlike awaiting a tokio
        // `JoinHandle` or a `smol::channel` whose wakeups may not cross
        // executors reliably).
        let tunnel_id_for_set = tunnel_id;
        let (tx, rx) = tokio::sync::oneshot::channel::<
            Option<(std::sync::Arc<OwnedSession>, std::sync::Arc<TunnelManager>)>,
        >();

        crabport_ssh::TOKIO.spawn(async move {
            let verifier = None; // owned tunnels don't prompt for host keys
            // for now — reuse the app verifier later if needed.
            let session = match OwnedSession::connect(info, verifier).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("owned session connect failed: {e}");
                    let _ = tx.send(None);
                    return;
                }
            };
            let session_arc: std::sync::Arc<dyn CrabPortTunnel> = session.clone();
            let manager =
                std::sync::Arc::new(TunnelManager::new(session_arc, std::sync::Arc::new(|| {})));
            let start_result = match entry.kind {
                TunnelKind::Local => {
                    manager
                        .start_local(
                            entry.name.clone(),
                            entry.bind_addr.clone(),
                            entry.bind_port,
                            entry.target_host.clone(),
                            entry.target_port,
                        )
                        .await
                }
                TunnelKind::Remote => {
                    manager
                        .start_remote(
                            entry.name.clone(),
                            entry.bind_addr.clone(),
                            entry.bind_port,
                            entry.target_host.clone(),
                            entry.target_port,
                        )
                        .await
                }
                TunnelKind::Dynamic => {
                    manager
                        .start_dynamic(entry.name.clone(), entry.bind_addr.clone(), entry.bind_port)
                        .await
                }
            };
            let _ = tx.send(match start_result {
                Ok(_) => Some((session, manager)),
                Err(e) => {
                    tracing::error!("tunnel start failed: {e}");
                    None
                }
            });
        });

        cx.spawn(async move |this, cx| {
            let tunnel_name = tunnel_name.clone();
            match rx.await {
                Ok(Some((session, manager))) => {
                    let _ = this.update(cx, |app, cx| {
                        app.app_ctx
                            .tunnels
                            .set_owned(tunnel_id_for_set, session, manager);
                        app.app_ctx.notifications.update(cx, |c, cx| {
                            c.show(
                                Notification::new(t!("tunnels.notif_start_title").to_string())
                                    .level(NotificationLevel::Success)
                                    .message(
                                        t!("tunnels.notif_start_msg", name = tunnel_name.as_str())
                                            .to_string(),
                                    )
                                    .duration(std::time::Duration::from_secs(3)),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                }
                _ => {
                    let _ = this.update(cx, |app, cx| {
                        app.app_ctx.notifications.update(cx, |c, cx| {
                            c.show(
                                Notification::new(
                                    t!("tunnels.notif_start_failed_title").to_string(),
                                )
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!(
                                        "tunnels.notif_start_failed_msg",
                                        name = tunnel_name.as_str()
                                    )
                                    .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Stop a running tunnel (owned or borrowed).
    pub fn stop_tunnel(&mut self, tunnel_id: i64, cx: &mut Context<Self>) {
        let manager = self.app_ctx.tunnels.manager_for(tunnel_id);
        let Some(manager) = manager else {
            return;
        };
        let tunnel_name = AppState::store(cx)
            .lock()
            .find_tunnel(tunnel_id)
            .ok()
            .flatten()
            .map(|e| e.name)
            .unwrap_or_else(|| tunnel_id.to_string());
        let id = tunnel_id;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        crabport_ssh::TOKIO.spawn(async move {
            manager.stop_all().await;
            let _ = tx.send(());
        });
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            let _ = this.update(cx, |app, cx| {
                app.app_ctx.tunnels.clear_runtime(id);
                app.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("tunnels.notif_stop_title").to_string())
                            .level(NotificationLevel::Success)
                            .message(
                                t!("tunnels.notif_stop_msg", name = tunnel_name.as_str())
                                    .to_string(),
                            )
                            .duration(std::time::Duration::from_secs(3)),
                        cx,
                    );
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Start a tunnel by borrowing the active terminal tab's SSH connection
    /// (started from the Tunnels panel). Unlike [`start_tunnel_owned`], no
    /// dedicated SSH session is opened — the tunnel reuses the active tab's
    /// backend, so the host must already be connected. The tunnel is torn
    /// down automatically when that tab closes (see `TunnelRegistry::teardown_for_tab`).
    ///
    /// Mutual exclusion: a tunnel can only run from one source at a time.
    /// If the tunnel is already running (owned or borrowed) the start is
    /// rejected with a warning toast.
    pub fn start_tunnel_borrowed(&mut self, tunnel_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        if self.app_ctx.tunnels.is_running(tunnel_id) {
            self.app_ctx.notifications.update(cx, |c, cx| {
                c.show(
                    Notification::new(t!("tunnels.notif_already_running_title").to_string())
                        .level(NotificationLevel::Warning)
                        .message(t!("tunnels.notif_already_running_msg").to_string())
                        .duration(std::time::Duration::from_secs(3)),
                    cx,
                );
            });
            tracing::warn!("tunnel {tunnel_id} already running");
            return;
        }
        // Resolve the tunnel config.
        let store = AppState::store(cx);
        let entry = match store.lock().find_tunnel(tunnel_id) {
            Ok(Some(e)) => e,
            _ => return,
        };
        let tunnel_name = entry.name.clone();

        // Borrow the active terminal's tunnel source. The terminal must be an
        // SSH session (local PTY backends expose no tunnel source).
        let Some(term_entity) = self.terminal_views.get(&tab_id).cloned() else {
            tracing::warn!("start_tunnel_borrowed: tab {tab_id} not found");
            return;
        };
        let source = term_entity.read_with(cx, |v, _| v.tunnel_source().cloned());
        let Some(source) = source else {
            self.app_ctx.notifications.update(cx, |c, cx| {
                c.show(
                    Notification::new(t!("tunnels.notif_borrow_no_session_title").to_string())
                        .level(NotificationLevel::Warning)
                        .message(t!("tunnels.notif_borrow_no_session_msg").to_string())
                        .duration(std::time::Duration::from_secs(4)),
                    cx,
                );
            });
            return;
        };

        // Build a TunnelManager backed by the borrowed source. The
        // `on_change` callback is a no-op (matching `start_tunnel_owned`):
        // tunnel state is surfaced via the `cx.notify()` calls in the
        // start/stop completion handlers, and the `TunnelManager`'s
        // `on_change` is `Send + Sync` which doesn't compose with GPUI's
        // thread-local entity handles.
        let manager = Arc::new(TunnelManager::new(source, Arc::new(|| {})));

        // Register the borrowed runtime up front so the panel immediately
        // reflects the "running" state and `stop_tunnel` can find the manager.
        // The manager is `Arc`-backed, so the clone held by the registry keeps
        // the tunnels alive even after the spawn task below drops its clone.
        self.app_ctx
            .tunnels
            .set_borrowed(tunnel_id, tab_id, manager.clone());

        let tunnel_id_for_set = tunnel_id;
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        crabport_ssh::TOKIO.spawn(async move {
            let start_result = match entry.kind {
                TunnelKind::Local => {
                    manager
                        .start_local(
                            entry.name.clone(),
                            entry.bind_addr.clone(),
                            entry.bind_port,
                            entry.target_host.clone(),
                            entry.target_port,
                        )
                        .await
                }
                TunnelKind::Remote => {
                    manager
                        .start_remote(
                            entry.name.clone(),
                            entry.bind_addr.clone(),
                            entry.bind_port,
                            entry.target_host.clone(),
                            entry.target_port,
                        )
                        .await
                }
                TunnelKind::Dynamic => {
                    manager
                        .start_dynamic(entry.name.clone(), entry.bind_addr.clone(), entry.bind_port)
                        .await
                }
            };
            let _ = tx.send(start_result.is_ok());
            // `manager` (the spawn's clone) drops here — the registry's clone
            // keeps the live tunnels alive.
        });

        cx.spawn(async move |this, cx| {
            let tunnel_name = tunnel_name.clone();
            match rx.await {
                Ok(true) => {
                    let _ = this.update(cx, |app, cx| {
                        app.app_ctx.notifications.update(cx, |c, cx| {
                            c.show(
                                Notification::new(t!("tunnels.notif_start_title").to_string())
                                    .level(NotificationLevel::Success)
                                    .message(
                                        t!("tunnels.notif_start_msg", name = tunnel_name.as_str())
                                            .to_string(),
                                    )
                                    .duration(std::time::Duration::from_secs(3)),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                }
                _ => {
                    let _ = this.update(cx, |app, cx| {
                        // Start failed — tear down the borrowed runtime we
                        // optimistically registered above.
                        app.app_ctx.tunnels.clear_runtime(tunnel_id_for_set);
                        app.app_ctx.notifications.update(cx, |c, cx| {
                            c.show(
                                Notification::new(
                                    t!("tunnels.notif_start_failed_title").to_string(),
                                )
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!(
                                        "tunnels.notif_start_failed_msg",
                                        name = tunnel_name.as_str()
                                    )
                                    .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn remove_tunnel(&mut self, tunnel_id: i64, cx: &mut Context<Self>) {
        let store = AppState::store(cx);
        let tunnels = self.app_ctx.tunnels.clone();
        // `tunnels.remove` calls `manager.stop_all().await` (tokio I/O) — run
        // on `TOKIO`, signal completion via oneshot.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        crabport_ssh::TOKIO.spawn(async move {
            tunnels.remove(tunnel_id).await;
            let _ = store.lock().remove_tunnel(tunnel_id);
            let _ = tx.send(());
        });
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    /// Toggle the favorite flag on a tunnel config. Persists via the store,
    /// then mirrors the change into the registry in place (preserving any
    /// live runtime) so the list re-sorts with favorites floating to the top
    /// — without rebuilding the whole registry list.
    pub fn toggle_tunnel_favorite(&mut self, tunnel_id: i64, cx: &mut Context<Self>) {
        let store = AppState::store(cx);
        match store.lock().toggle_tunnel_favorite(tunnel_id) {
            Ok(new_val) => {
                self.app_ctx.tunnels.set_favorite(tunnel_id, new_val);
            }
            Err(e) => {
                tracing::error!("toggle_tunnel_favorite failed: {e}");
            }
        }
        cx.notify();
    }

    /// Move a tunnel to a different group (`None` = ungrouped). Persists via
    /// the store, then mirrors into the registry in place.
    pub fn set_tunnel_group(
        &mut self,
        tunnel_id: i64,
        group_id: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let store = AppState::store(cx);
        if let Err(e) = store.lock().set_tunnel_group(tunnel_id, group_id) {
            tracing::error!("set_tunnel_group failed: {e}");
        }
        self.app_ctx.tunnels.set_group(tunnel_id, group_id);
        cx.notify();
    }
}
