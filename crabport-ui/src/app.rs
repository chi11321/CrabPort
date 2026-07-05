use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use rust_i18n::t;

use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::AlertController;
use crate::components::notification::{
    Notification, NotificationController, NotificationLevel, NotificationPosition,
};
use crate::layouts::command_palette::{CommandView, ConnectionType};
use crate::layouts::sidebar::render_sidebar;
use crate::views::hosts::ConnectionHost;
use crate::views::hosts::{AuthKind, ConnectionFormState, ConnectionKind};
use crate::views::terminal::TerminalView;
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
    ProxyConfig, ProxyEntry, TunnelEntry, TunnelKind,
};
use crabport_ssh::backend::SshBackend;
use crabport_ssh::session::SshConnectionInfo;
use crabport_ssh::{CrabPortTunnel, OwnedSession, TunnelManager};
use crabport_terminal::terminal::SftpTransferKind;

use crate::app_state::AppState;

// ---- CrabPortTab trait ----

pub trait CrabPortTab: 'static {
    fn close(&mut self);
}

// ---- App ----

actions!(app, [ToggleCommand, TerminalTab, TerminalShiftTab]);

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    pub is_remote: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabKind {
    Home,
    Terminal,
}

pub struct CrabportApp {
    pub sidebar_item: SidebarItem,
    pub tabs: Vec<Tab>,
    pub active_tab_id: u64,
    pub hovered_tab_id: Option<u64>,
    pub next_tab_id: u64,
    pub terminal_views: HashMap<u64, Entity<TerminalView>>,
    pub hosts: Vec<ConnectionHost>,
    pub connection_form: Option<ConnectionFormState>,
    pub command_palette: Entity<CommandView>,
    pub sftp_panel: Entity<crate::views::panel::sftp::SftpPanel>,
    pub snippets_panel: Entity<crate::views::panel::snippets_panel::SnippetsPanel>,
    pub history_panel: Entity<crate::views::panel::history_command_panel::HistoryCommandPanel>,
    /// Tunnels side panel (borrowed tunnels reusing the active tab's SSH
    /// connection). Only useful for SSH tabs.
    pub tunnels_panel: Entity<crate::views::panel::tunnels_panel::TunnelsPanel>,
    /// Active index of the right-hand panel's tab strip (SFTP / History /
    /// Snippets). Driven by `Tabs::on_change` in `render_panel`.
    pub panel_active_tab: usize,
    pub hosts_view: Entity<crate::views::hosts::HostsView>,
    /// Snippets management sidebar view (right-click edit/delete).
    pub snippets_view: Entity<crate::views::snippets::SnippetsView>,
    /// Tunnels management sidebar view (create/start/stop/edit/delete).
    pub tunnels_view: Entity<crate::views::tunnels::TunnelsView>,
    /// Global alert dialog host. Rendered at the app root so it overlays the
    /// entire window regardless of which view is active. Triggered via
    /// `alert_controller.update(cx, |c, cx| c.show(state, cx))`.
    pub alert_controller: Entity<AlertController>,
    /// Global context menu host. Triggered via
    /// `context_menu.update(cx, |c, cx| c.show(state, cx))`.
    pub context_menu: Entity<ContextMenuController>,
    /// Global toast notification host. Rendered at the app root so toasts
    /// overlay the entire window regardless of which view is active.
    /// Triggered via
    /// `notification_controller.update(cx, |c, cx| c.show(notification, cx))`.
    pub notification_controller: Entity<NotificationController>,
    /// Central registry of all tunnels (stopped + running). Single source
    /// of truth for the Tunnels view; mutations fire `cx.notify()` via the
    /// `on_change` callback wired at construction.
    pub tunnels: Arc<crate::views::tunnels::TunnelRegistry>,
    /// Tunnel form window state (singleton dialog for creating/editing a
    /// tunnel config). `None` when the dialog is closed.
    pub tunnel_form: Option<crate::views::tunnels::TunnelFormState>,
    wired: bool,
    /// Tab id that currently holds focus. Used to focus the terminal only on
    /// actual tab switches instead of every render (which would steal focus
    /// from overlays like SFTP/command palette/connection form).
    last_focused_tab_id: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarItem {
    Sessions,
    Tunnels,
    Snippets,
    History,
}

impl SidebarItem {
    pub fn label(&self) -> SharedString {
        match self {
            SidebarItem::Sessions => t!("sidebar.sessions").into(),
            SidebarItem::Tunnels => t!("sidebar.tunnels").into(),
            SidebarItem::Snippets => t!("sidebar.snippets").into(),
            SidebarItem::History => t!("sidebar.history").into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            SidebarItem::Sessions => "icons/monitor-cloud.svg",
            SidebarItem::Tunnels => "icons/waypoints.svg",
            SidebarItem::Snippets => "icons/braces.svg",
            SidebarItem::History => "icons/clock.svg",
        }
    }

    pub fn all() -> [SidebarItem; 4] {
        [
            SidebarItem::Sessions,
            SidebarItem::Tunnels,
            SidebarItem::Snippets,
            SidebarItem::History,
        ]
    }
}

impl CrabportApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home_tab = Tab {
            id: 0,
            title: "Home".into(),
            kind: TabKind::Home,
            is_remote: false,
        };

        let command_palette = cx.new(|cx| CommandView::new(window, cx));
        let sftp_panel = cx.new(|_cx| crate::views::panel::sftp::SftpPanel::new());
        let snippets_panel =
            cx.new(|_cx| crate::views::panel::snippets_panel::SnippetsPanel::new());
        let history_panel =
            cx.new(|_cx| crate::views::panel::history_command_panel::HistoryCommandPanel::new());
        let tunnels_panel = cx.new(|_cx| crate::views::panel::tunnels_panel::TunnelsPanel::new());
        let app_entity = cx.entity();
        let hosts_view = cx.new(|_cx| crate::views::hosts::HostsView::new(app_entity.clone()));
        let snippets_view = cx.new(|_cx| crate::views::snippets::SnippetsView::new());
        let tunnels_view = cx.new(|_cx| crate::views::tunnels::TunnelsView::new(app_entity));
        let alert_controller = cx.new(|_cx| AlertController::new());
        let context_menu = cx.new(|_cx| ContextMenuController::new());
        let notification_controller =
            cx.new(|_cx| NotificationController::new(NotificationPosition::BottomRight));

        // Tunnel registry: a plain mutex-guarded list of tunnel configs +
        // their optional runtime state. `CrabportApp` calls `cx.notify()`
        // after each mutation (start/stop/add/remove) since those run in
        // GPUI contexts. The registry itself is context-free.
        let tunnels = Arc::new(crate::views::tunnels::TunnelRegistry::new());

        // Read persisted data through the shared global store. The global
        // is initialized in `main` before any window is opened.
        let store = AppState::store(cx);
        let hosts: Vec<ConnectionHost> = store
            .lock()
            .hosts()
            .unwrap_or_default()
            .into_iter()
            .map(|h| ConnectionHost {
                id: h.id,
                name: h.name,
                host: h.host,
                port: h.port,
                username: h.username,
                kind: match h.kind {
                    CoreHostKind::Ssh => crate::views::hosts::ConnectionKind::SSH,
                    CoreHostKind::Telnet => crate::views::hosts::ConnectionKind::Telnet,
                    CoreHostKind::Serial => crate::views::hosts::ConnectionKind::Serial,
                },
                credential_id: h.credential_id,
                last_login: h.last_login,
                favorite: h.favorite,
                proxy_id: h.proxy_id,
            })
            .collect();

        // Load persisted tunnel configs from the store. Tunnels start in the
        // stopped state — the user starts them explicitly from the Tunnels
        // view or a terminal panel.
        let tunnel_configs = store.lock().tunnels().unwrap_or_default();
        tunnels.set_configs(tunnel_configs);

        Self {
            sidebar_item: SidebarItem::Sessions,
            tabs: vec![home_tab],
            active_tab_id: 0,
            hovered_tab_id: None,
            next_tab_id: 1,
            terminal_views: HashMap::new(),
            hosts,
            connection_form: None,
            command_palette,
            sftp_panel,
            snippets_panel,
            history_panel,
            tunnels_panel,
            panel_active_tab: 0,
            hosts_view,
            snippets_view,
            tunnels_view,
            alert_controller,
            context_menu,
            notification_controller,
            tunnels,
            tunnel_form: None,
            wired: false,
            last_focused_tab_id: None,
        }
    }

    /// Wire cross-entity callbacks. Called once after construction.
    pub fn wire(&mut self, cx: &mut Context<Self>) {
        if self.wired {
            return;
        }
        self.wired = true;

        let cmd = self.command_palette.clone();
        let app = cx.entity().clone();

        // ---- Command palette callbacks ----
        let cmd_for_close = cmd.clone();
        let cmd_for_new = cmd.clone();
        let app_for_cmd = app.clone();
        self.command_palette.update(cx, move |cmd, _cx| {
            cmd.set_on_close({
                let c = cmd_for_close.clone();
                move |_, cx| {
                    c.update(cx, |cmd, cx| cmd.close(cx));
                }
            });
            cmd.set_on_new_connection({
                let c = cmd_for_new.clone();
                let a = app_for_cmd.clone();
                move |ct, w, cx| {
                    match ct {
                        ConnectionType::LocalTerminal => {
                            a.update(cx, |app, cx| {
                                app.add_tab(cx);
                            });
                        }
                        _ => {
                            a.update(cx, |app, _cx| {
                                app.activate_tab(0);
                                app.sidebar_item = SidebarItem::Sessions;
                            });
                            a.update(cx, |app, cx| {
                                app.open_connection_form(w, cx);
                            });
                        }
                    }
                    c.update(cx, |cmd, cx| cmd.close(cx));
                }
            });
            cmd.set_on_select_host({
                let c = cmd_for_new.clone();
                let a = app_for_cmd.clone();
                move |idx, _w, cx| {
                    a.update(cx, |app, cx| {
                        let host_id = app.hosts.get(idx).map(|h| h.id).unwrap_or(-1);
                        if host_id >= 0 {
                            app.connect_to_host(host_id, cx);
                        }
                    });
                    c.update(cx, |cmd, cx| cmd.close(cx));
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // Connection form (ephemeral entity — created on open, destroyed after close animation)
    // -----------------------------------------------------------------------

    /// Create a new ConnectionFormView entity, wire its callbacks, and open it.
    pub fn open_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // If one is already open, just bring it to front
        if let Some(ref mut form) = self.connection_form {
            form.open(window, cx);
            cx.notify();
            return;
        }

        let mut form = ConnectionFormState::new(window, cx);
        let app = cx.entity().clone();

        form.on_close = Some(Rc::new({
            let a = app.clone();
            move |_: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    app.close_connection_form(cx);
                });
            }
        }));

        form.on_connect = Some(Rc::new({
            let a = app.clone();
            move |_kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    // Validate required fields before doing anything. If the
                    // form is invalid, per-field errors are shown and a toast
                    // is surfaced; the save/connect flow is aborted.
                    if !app.validate_connection_form(cx) {
                        return;
                    }
                    // Read form values directly from state
                    let (
                        name,
                        host,
                        port_num,
                        username,
                        password,
                        passphrase,
                        auth_kind,
                        private_key,
                        proxy_config,
                    ) = {
                        let f = app.connection_form.as_ref().unwrap();
                        let n = f.name_text(cx);
                        let h = f.host_text(cx);
                        let p: u16 = f.port_text(cx).parse().unwrap_or(22);
                        let u = f.user_text(cx);
                        let pw = f.pass_text(cx);
                        let pp = f.passphrase_text(cx);
                        let ak = f.auth_kind;
                        let pk = f.private_key_text(cx);
                        let pc = f.proxy_config(cx);
                        (n, h, p, u, pw, pp, ak, pk, pc)
                    };
                    app.close_connection_form(cx);

                    // Persist credential for this host
                    let (cred_kind, secret, pk) = match auth_kind {
                        AuthKind::Password => (
                            CoreCredentialKind::Password,
                            password.clone(),
                            String::new(),
                        ),
                        AuthKind::Certificate => (
                            CoreCredentialKind::Certificate,
                            passphrase.clone(),
                            private_key.clone(),
                        ),
                    };
                    let cred = CredentialEntry {
                        id: 0,
                        name: name.clone(),
                        kind: cred_kind,
                        anonymous: true,
                        secret,
                        private_key: pk,
                        public_key: String::new(),
                        certificate: String::new(),
                    };
                    let cred_id = AppState::store(cx)
                        .lock()
                        .add_credential(&cred)
                        .unwrap_or(0);

                    // Persist host with linked credential
                    let proxy_id = upsert_proxy_for_host(&proxy_config, None, cx);
                    let entry = HostEntry {
                        id: 0,
                        name: name.clone(),
                        host: host.clone(),
                        port: port_num,
                        username: username.clone(),
                        credential_id: Some(cred_id),
                        kind: CoreHostKind::Ssh,
                        last_login: None,
                        favorite: false,
                        proxy_id,
                    };
                    let row_id = AppState::store(cx).lock().add_host(&entry).unwrap_or(0);

                    app.hosts.push(ConnectionHost {
                        id: row_id,
                        name: name.clone(),
                        host: host.to_string(),
                        port: port_num,
                        username: username.to_string(),
                        kind: crate::views::hosts::ConnectionKind::SSH,
                        credential_id: Some(cred_id),
                        last_login: None,
                        favorite: false,
                        proxy_id,
                    });
                    let (private_key_arg, passphrase_arg) = match auth_kind {
                        AuthKind::Password => (None, None),
                        AuthKind::Certificate => (
                            if private_key.is_empty() {
                                None
                            } else {
                                Some(private_key.as_str())
                            },
                            if passphrase.is_empty() {
                                None
                            } else {
                                Some(passphrase.as_str())
                            },
                        ),
                    };
                    app.add_ssh_tab(
                        &name,
                        Some(row_id),
                        &host,
                        port_num,
                        &username,
                        match auth_kind {
                            AuthKind::Password => &password,
                            AuthKind::Certificate => "",
                        },
                        private_key_arg,
                        passphrase_arg,
                        proxy_config,
                        cx,
                    );
                    cx.notify();
                });
            }
        }));

        form.open(window, cx);
        self.connection_form = Some(form);
        cx.notify();
    }

    /// Close the connection form. The state stays alive for the exit animation,
    /// then is destroyed by a timer.
    pub fn close_connection_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.connection_form {
            form.close();
        } else {
            return;
        }
        // After animation finishes, destroy the state and clean up animations
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.connection_form.is_some() {
                    // Clean up Tabs animation state (conn-auth-tabs has 2 panes)
                    let tabs_id = ElementId::Name("conn-auth-tabs".into());
                    crate::components::tabs::Tabs::cleanup_animation(&tabs_id, 2);
                    app.connection_form = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Validate the connection form before submitting. Populates per-field
    /// error state (rendered via `StyledInput.error(...)`) and, when invalid,
    /// shows a toast notification summarizing the missing fields. Returns
    /// `true` when the form is valid and the caller may proceed with the
    /// save / connect flow.
    pub fn validate_connection_form(&mut self, cx: &mut Context<Self>) -> bool {
        let valid = self
            .connection_form
            .as_mut()
            .map(|form| form.validate(cx))
            .unwrap_or(true);
        if !valid {
            // Surface a summary toast so the user knows something is wrong
            // even if the offending field is scrolled out of view.
            self.notification_controller.update(cx, |c, cx| {
                c.show(
                    Notification::new(t!("connection_form.validation_title").to_string())
                        .level(NotificationLevel::Warning)
                        .message(t!("connection_form.validation_message").to_string())
                        .duration(std::time::Duration::from_secs(4)),
                    cx,
                );
            });
            cx.notify();
        }
        valid
    }

    // -----------------------------------------------------------------------
    // Tunnels
    // -----------------------------------------------------------------------

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
        if self.tunnels.is_running(tunnel_id) {
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
        };
        let store = AppState::store(cx);
        match out.editing_id {
            Some(_id) => {
                if let Err(e) = store.lock().update_tunnel(&entry) {
                    tracing::error!("update_tunnel failed: {e}");
                    self.notification_controller.update(cx, |c, cx| {
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
                self.tunnels.update_config(entry);
                self.notification_controller.update(cx, |c, cx| {
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
                        self.notification_controller.update(cx, |c, cx| {
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
                self.tunnels.add(entry);
                self.notification_controller.update(cx, |c, cx| {
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
        if self.tunnels.is_running(tunnel_id) {
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
                        app.tunnels.set_owned(tunnel_id_for_set, session, manager);
                        app.notification_controller.update(cx, |c, cx| {
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
                        app.notification_controller.update(cx, |c, cx| {
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
        let manager = self.tunnels.manager_for(tunnel_id);
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
                app.tunnels.clear_runtime(id);
                app.notification_controller.update(cx, |c, cx| {
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
        if self.tunnels.is_running(tunnel_id) {
            self.notification_controller.update(cx, |c, cx| {
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
            self.notification_controller.update(cx, |c, cx| {
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
        self.tunnels
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
                        app.notification_controller.update(cx, |c, cx| {
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
                        app.tunnels.clear_runtime(tunnel_id_for_set);
                        app.notification_controller.update(cx, |c, cx| {
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
        let tunnels = self.tunnels.clone();
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

    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    pub fn is_home_active(&self) -> bool {
        self.tabs
            .iter()
            .find(|t| t.id == self.active_tab_id)
            .map(|t| t.kind == TabKind::Home)
            .unwrap_or(false)
    }

    pub fn add_tab(&mut self, cx: &mut Context<Self>) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title: format!("Terminal-{}", id),
            kind: TabKind::Terminal,
            is_remote: false,
        });

        let terminal_view = cx.new(|cx| TerminalView::new(id, cx));

        // When the local PTY child exits, automatically close the tab
        let app_handle = cx.entity().clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_tab(id, cx);
                });
            });
        });

        // Re-render the app when SFTP transfer progress changes so the
        // toolbar (rendered in `render_content`) picks up the latest
        // snapshot. We use a dedicated callback rather than observing the
        // whole view to avoid re-rendering the app on every terminal frame
        // pump tick (~120Hz during output).
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_progress_changed(move |cx| {
                let _ = app_handle.update(cx, |_, cx| cx.notify());
            });
        });

        // Surface a toast notification when an SFTP transfer finishes so the
        // user gets clear success/failure feedback even if the SFTP panel is
        // closed or scrolled out of view.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_transfer_finished(move |kind, success, message, cx| {
                let _ = app_handle.update(cx, |app, cx| {
                    let (title, message_notif, level, duration) = match (kind, success) {
                        (SftpTransferKind::Download, true) => (
                            t!("sftp.notif_download_done_title").to_string(),
                            t!("sftp.notif_download_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Download, false) => (
                            t!("sftp.notif_download_failed_title").to_string(),
                            t!("sftp.notif_download_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Upload, true) => (
                            t!("sftp.notif_upload_done_title").to_string(),
                            t!("sftp.notif_upload_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Upload, false) => (
                            t!("sftp.notif_upload_failed_title").to_string(),
                            t!("sftp.notif_upload_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                    };
                    app.notification_controller.update(cx, |c, cx| {
                        c.show(
                            Notification::new(title)
                                .level(level)
                                .message(message_notif)
                                .duration(duration),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
        });

        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn add_ssh_tab(
        &mut self,
        name: &str,
        host_id: Option<i64>,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        private_key: Option<&str>,
        passphrase: Option<&str>,
        proxy: Option<crabport_core::credential::ProxyConfig>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title: name.to_string(),
            kind: TabKind::Terminal,
            is_remote: true,
        });

        let mut info = SshConnectionInfo::new(host, username, password).with_port(port);
        if let Some(pk) = private_key {
            info = info.with_private_key(pk, passphrase.map(|s| s.to_string()));
        }
        if let Some(p) = proxy {
            info = info.with_proxy(p);
        }
        let info_for_view = info.clone();
        let cols: usize = 80;
        let rows: usize = 24;

        // Create the overlay state early so the SSH backend callback can write to it
        let overlay: crate::views::terminal::connection_overlay::SharedOverlayState =
            std::sync::Arc::new(parking_lot::Mutex::new(
                crate::views::terminal::connection_overlay::ConnectionOverlayState::new(),
            ));
        let overlay_cb = overlay.clone();

        // Host-key verifier: pushes a confirmation prompt into the overlay
        // when the server presents an unknown key, and awaits the user's
        // decision (TOFU). See `make_host_key_verifier` for the repaint
        // mechanism.
        let verifier =
            crate::views::terminal::connection_overlay::make_host_key_verifier(overlay.clone());

        let backend = Arc::new(SshBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(
                    crate::views::terminal::connection_overlay::ConnectionLogLevel::Info,
                    msg,
                );
            }),
            Some(verifier),
        ));
        // Clone the backend as a `CrabPortTunnel` source before it's moved
        // into the `TerminalView` (coerced to `Arc<dyn CrabPortTerminal>`).
        // `SshBackend` implements `CrabPortTunnel`, so the panel can reuse
        // this tab's SSH connection for borrowed tunnels.
        let tunnel_source: Arc<dyn crabport_ssh::CrabPortTunnel> = backend.clone();
        let terminal_view = cx.new(|cx| {
            TerminalView::with_backend_and_host_and_overlay(
                backend,
                cols,
                rows,
                format!("{}@{}", username, host),
                host_id,
                overlay,
                Some(info_for_view),
                id,
                cx,
            )
        });
        // When the SSH session closes, automatically close the tab
        let app_handle = cx.entity().clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_tab(id, cx);
                });
            });
        });

        // Re-render the app when SFTP transfer progress changes so the
        // toolbar picks up the latest snapshot.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_progress_changed(move |cx| {
                let _ = app_handle.update(cx, |_, cx| cx.notify());
            });
        });

        // Surface a toast notification when an SFTP transfer finishes so the
        // user gets clear success/failure feedback even if the SFTP panel is
        // closed or scrolled out of view.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_transfer_finished(move |kind, success, message, cx| {
                let _ = app_handle.update(cx, |app, cx| {
                    let (title, message_notif, level, duration) = match (kind, success) {
                        (SftpTransferKind::Download, true) => (
                            t!("sftp.notif_download_done_title").to_string(),
                            t!("sftp.notif_download_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Download, false) => (
                            t!("sftp.notif_download_failed_title").to_string(),
                            t!("sftp.notif_download_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Upload, true) => (
                            t!("sftp.notif_upload_done_title").to_string(),
                            t!("sftp.notif_upload_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Upload, false) => (
                            t!("sftp.notif_upload_failed_title").to_string(),
                            t!("sftp.notif_upload_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                    };
                    app.notification_controller.update(cx, |c, cx| {
                        c.show(
                            Notification::new(title)
                                .level(level)
                                .message(message_notif)
                                .duration(duration),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
        });

        // Wire the `CrabPortTunnel` source captured above into the view so
        // the Tunnels panel can start borrowed tunnels reusing this tab's
        // SSH connection.
        terminal_view.update(cx, |view, _cx| {
            view.set_tunnel_source(tunnel_source);
        });

        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn activate_tab(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_tab_id = id;
        }
    }

    /// Connect to a saved host by ID. Resolves the linked credential password.
    pub fn connect_to_host(&mut self, host_id: i64, cx: &mut Context<Self>) {
        let host = match self.hosts.iter().find(|h| h.id == host_id) {
            Some(h) => h.clone(),
            None => return,
        };

        // Update last_login timestamp
        let _ = AppState::store(cx).lock().touch_host_login(host_id);
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
                        CoreHostKind::Ssh => crate::views::hosts::ConnectionKind::SSH,
                        CoreHostKind::Telnet => crate::views::hosts::ConnectionKind::Telnet,
                        CoreHostKind::Serial => crate::views::hosts::ConnectionKind::Serial,
                    },
                    credential_id: h.credential_id,
                    last_login: h.last_login,
                    favorite: h.favorite,
                    proxy_id: h.proxy_id,
                })
                .collect();
        }

        // Try to resolve password and private key from linked credential
        let cred = host.credential_id.and_then(|cid| {
            AppState::store(cx)
                .lock()
                .find_credential(cid)
                .ok()
                .flatten()
        });

        // Resolve password / passphrase based on credential kind
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

        // Resolve the saved proxy (if any) into a ready-to-use config.
        let proxy_config = host.proxy_id.and_then(|pid| {
            AppState::store(cx)
                .lock()
                .find_proxy_config(pid)
                .ok()
                .flatten()
        });

        self.add_ssh_tab(
            &host.name,
            Some(host_id),
            &host.host,
            host.port,
            &host.username,
            &password,
            private_key,
            passphrase,
            proxy_config,
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Host management
    // -----------------------------------------------------------------------

    pub fn remove_host(&mut self, host_id: i64, cx: &mut Context<Self>) {
        let host = self.hosts.iter().find(|h| h.id == host_id);
        if let Some(h) = host {
            if let Some(cred_id) = h.credential_id {
                let _ = AppState::store(cx).lock().remove_credential(cred_id);
            }
        }
        let _ = AppState::store(cx).lock().remove_host(host_id);
        self.hosts.retain(|h| h.id != host_id);
        cx.notify();
    }

    pub fn edit_host(&mut self, host_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.close_connection_form(cx);

        let host = self.hosts.iter().find(|h| h.id == host_id).cloned();
        if host.is_none() {
            return;
        }
        let h = host.unwrap();

        let cred = h.credential_id.and_then(|cid| {
            AppState::store(cx)
                .lock()
                .find_credential(cid)
                .ok()
                .flatten()
        });

        let mut form = ConnectionFormState::new(window, cx);

        form.name_input.update(cx, |state, cx| {
            state.set_value(&h.name, window, cx);
        });
        form.host_input.update(cx, |state, cx| {
            state.set_value(&h.host, window, cx);
        });
        form.port_input.update(cx, |state, cx| {
            state.set_value(&h.port.to_string(), window, cx);
        });
        form.user_input.update(cx, |state, cx| {
            state.set_value(&h.username, window, cx);
        });

        if let Some(c) = cred.as_ref() {
            match c.kind {
                CoreCredentialKind::Password => {
                    form.pass_input.update(cx, |state, cx| {
                        state.set_value(&c.secret, window, cx);
                    });
                }
                CoreCredentialKind::Certificate => {
                    form.auth_kind = AuthKind::Certificate;
                    form.passphrase_input.update(cx, |state, cx| {
                        state.set_value(&c.secret, window, cx);
                    });
                    form.private_key_input.update(cx, |state, cx| {
                        state.set_value(&c.private_key, window, cx);
                    });
                }
            }
        }

        // Load the saved proxy (if any) so the user can edit / clear it.
        // System proxies aren't persisted, so we only resolve a config for a
        // real `proxy_id`.
        let saved_proxy = h.proxy_id.and_then(|pid| {
            AppState::store(cx)
                .lock()
                .find_proxy_config(pid)
                .ok()
                .flatten()
                .map(|cfg| (pid, cfg))
        });
        #[cfg(debug_assertions)]
        tracing::info!(
            "edit_host: host_id={}, host.proxy_id={:?}, resolved_saved_proxy={}",
            h.id,
            h.proxy_id,
            saved_proxy.is_some()
        );
        match saved_proxy {
            Some((pid, cfg)) => form.load_proxy(Some(pid), Some(&cfg), window, cx),
            None => form.load_proxy(None, None, window, cx),
        }

        let app = cx.entity().clone();
        let editing_host_id = h.id;
        let editing_cred_id = h.credential_id;
        let editing_proxy_id = h.proxy_id;

        form.editing = true;

        form.on_connect = Some(Rc::new({
            let app = app.clone();
            move |_kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                #[cfg(debug_assertions)]
                tracing::info!(
                    "edit_host: on_connect fired — editing_proxy_id={:?}",
                    editing_proxy_id
                );
                app.update(cx, |app, cx| {
                    // Validate required fields before doing anything. If the
                    // form is invalid, per-field errors are shown and a toast
                    // is surfaced; the save flow is aborted.
                    if !app.validate_connection_form(cx) {
                        return;
                    }
                    let (
                        name,
                        host,
                        port_num,
                        username,
                        password,
                        passphrase,
                        auth_kind,
                        private_key,
                        proxy_config,
                    ) = {
                        let f = app.connection_form.as_ref().unwrap();
                        #[cfg(debug_assertions)]
                        tracing::info!(
                            "edit_host: reading form — proxy_kind={:?}, form.proxy_id={:?}, proxy_url={:?}",
                            f.proxy_kind,
                            f.proxy_id,
                            f.proxy_url_text(cx)
                        );
                        let n = f.name_text(cx);
                        let h = f.host_text(cx);
                        let p: u16 = f.port_text(cx).parse().unwrap_or(22);
                        let u = f.user_text(cx);
                        let pw = f.pass_text(cx);
                        let pp = f.passphrase_text(cx);
                        let ak = f.auth_kind;
                        let pk = f.private_key_text(cx);
                        let pc = f.proxy_config(cx);
                        (n, h, p, u, pw, pp, ak, pk, pc)
                    };
                    app.close_connection_form(cx);

                    let (cred_kind, secret, pk) = match auth_kind {
                        AuthKind::Password => (
                            CoreCredentialKind::Password,
                            password.clone(),
                            String::new(),
                        ),
                        AuthKind::Certificate => (
                            CoreCredentialKind::Certificate,
                            passphrase.clone(),
                            private_key.clone(),
                        ),
                    };

                    if let Some(old_cred_id) = editing_cred_id {
                        let _ = AppState::store(cx).lock().remove_credential(old_cred_id);
                    }

                    let cred = CredentialEntry {
                        id: 0,
                        name: name.clone(),
                        kind: cred_kind,
                        anonymous: true,
                        secret,
                        private_key: pk,
                        public_key: String::new(),
                        certificate: String::new(),
                    };
                    let new_cred_id = AppState::store(cx)
                        .lock()
                        .add_credential(&cred)
                        .unwrap_or(0);

                    let entry = HostEntry {
                        id: editing_host_id,
                        name: name.clone(),
                        host: host.clone(),
                        port: port_num,
                        username: username.clone(),
                        credential_id: Some(new_cred_id),
                        kind: CoreHostKind::Ssh,
                        last_login: None,
                        favorite: false,
                        proxy_id: upsert_proxy_for_host(&proxy_config, editing_proxy_id, cx),
                    };
                    #[cfg(debug_assertions)]
                    tracing::info!(
                        "edit_host: on_connect — editing_proxy_id={:?}, resolved_entry.proxy_id={:?}",
                        editing_proxy_id,
                        entry.proxy_id
                    );
                    let _ = AppState::store(cx).lock().update_host(&entry);

                    if let Some(h) = app.hosts.iter_mut().find(|h| h.id == editing_host_id) {
                        h.name = name.clone();
                        h.host = host.clone();
                        h.port = port_num;
                        h.username = username.clone();
                        h.credential_id = Some(new_cred_id);
                        h.proxy_id = entry.proxy_id;
                    }

                    cx.notify();
                });
            }
        }));

        form.on_close = Some(Rc::new({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    app.close_connection_form(cx);
                });
            }
        }));

        form.open(window, cx);
        self.connection_form = Some(form);
        cx.notify();
    }

    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if id == 0 {
            return;
        }

        // Find the tab before removing it, to know if it had a close button
        let tab = self.tabs.iter().find(|t| t.id == id);
        let is_home_tab = tab.map(|t| t.kind == TabKind::Home).unwrap_or(true);

        // Clean up gpui-animation state
        let tab_btn_id = ElementId::Name(format!("tab-{}", id).into());
        let tab_wrapper_id = ElementId::Name(format!("tab-wrapper-{}", id).into());
        Button::cleanup_animation(&tab_btn_id, !is_home_tab);
        gpui_animation::reset_transition(&tab_wrapper_id);

        if let Some(view) = self.terminal_views.remove(&id) {
            view.update(cx, |v, _cx| {
                v.close();
            });
        }
        self.tabs.retain(|t| t.id != id);
        if self.active_tab_id == id {
            self.active_tab_id = 0;
        }
    }

    pub fn toggle_command(
        &mut self,
        _: &ToggleCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cmd = self.command_palette.clone();
        let was_open = cmd.read(cx).open;
        cmd.update(cx, |cmd, cx| {
            if was_open {
                cmd.close(cx);
            } else {
                cmd.open(_window, cx);
            }
        });
        cx.notify();
    }

    // -- Helpers --
}

impl Render for CrabportApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let show_sidebar = self.is_home_active();

        // Host data for command palette (sorted by favorite desc, last_login desc, limited to 5)
        let mut sorted_hosts: Vec<ConnectionHost> = self.hosts.clone();
        sorted_hosts.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then_with(|| b.last_login.cmp(&a.last_login))
        });
        self.command_palette.update(cx, |cmd, _cx| {
            cmd.set_hosts(sorted_hosts);
        });

        // ---- Content view ----
        // Pre-read tunnel state here (in the render method, where `self` is
        // already borrowed) rather than via `handle.read_with` inside
        // `render_content` — that would be a nested read of `CrabportApp`
        // and panic ("cannot read while it is already being updated").
        // Same pattern as `panel_active_tab`.
        let tunnel_list = self.tunnels.list();
        let tunnel_form_state = self.tunnel_form.clone();

        let content = crate::layouts::content::render_content(
            self.sidebar_item,
            &handle,
            &self.tabs,
            self.active_tab_id,
            &self.terminal_views,
            &self.hosts,
            self.connection_form.as_ref(),
            &self.sftp_panel,
            &self.snippets_panel,
            &self.history_panel,
            &self.tunnels_panel,
            self.panel_active_tab,
            &self.hosts_view,
            &self.snippets_view,
            &self.tunnels_view,
            tunnel_list,
            tunnel_form_state,
            &self.alert_controller,
            &self.context_menu,
            &self.notification_controller,
            _window,
            cx,
        );

        // Focus the active terminal tab only when the active tab actually
        // changes — not on every render. Otherwise we'd steal focus from the
        // SFTP panel, command palette, connection form, etc.
        if self.last_focused_tab_id != Some(self.active_tab_id) {
            let active = self.tabs.iter().find(|t| t.id == self.active_tab_id);
            if let Some(tab) = active
                && tab.kind == TabKind::Terminal
                && let Some(entity) = self.terminal_views.get(&tab.id)
            {
                let fh = entity.read_with(cx, |view, cx| view.focus_handle(cx));
                _window.focus(&fh);
            }
            self.last_focused_tab_id = Some(self.active_tab_id);
        }

        // ---- Root ----
        div()
            .size_full()
            .bg(rgb(BG_BASE))
            .flex()
            .flex_row()
            .key_context("App")
            .on_action(cx.listener(Self::toggle_command))
            // -- Sidebar --
            .child(render_sidebar(self.sidebar_item, show_sidebar, &handle))
            .child(content)
            // -- Command palette --
            .child(self.command_palette.clone())
            // -- Global alert dialog (host-key prompts, etc.) --
            .child(self.alert_controller.clone())
            // -- Global context menu --
            .child(self.context_menu.clone())
            // -- Global toast notifications --
            .child(self.notification_controller.clone())
    }
}

// ---------------------------------------------------------------------------
// Main window construction
// ---------------------------------------------------------------------------

/// Persist (or update, or remove) the proxy row linked to a host.
///
/// - `proxy_config = None` → if `existing_id` was set, delete that proxy row
///   and return `None` (host becomes direct).
/// - `proxy_config = Some(cfg)` → if `existing_id` is set, update that row;
///   otherwise insert a new one. Returns the row id to store on the host.
///
/// The proxy is stored as an anonymous row (name = `"<host>"`) so it
/// doesn't clutter a future proxies-management UI.
fn upsert_proxy_for_host(
    proxy_config: &Option<ProxyConfig>,
    existing_id: Option<i64>,
    cx: &mut App,
) -> Option<i64> {
    #[cfg(debug_assertions)]
    tracing::info!(
        "upsert_proxy_for_host: existing_id={:?}, has_config={}",
        existing_id,
        proxy_config.is_some()
    );
    let store = AppState::store(cx);
    let proxy_config = proxy_config.as_ref()?;
    // Only persist enabled proxies (kind != None and host non-empty).
    if !proxy_config.is_enabled() {
        #[cfg(debug_assertions)]
        tracing::info!(
            "upsert_proxy_for_host: config not enabled (kind={:?}, host={:?}) — removing if set",
            proxy_config.kind,
            proxy_config.host
        );
        if let Some(id) = existing_id {
            let _ = store.lock().remove_proxy(id);
        }
        return None;
    }

    #[cfg(debug_assertions)]
    tracing::info!(
        "upsert_proxy_for_host: persisting kind={:?} {}:{} has_user={} has_pass={}",
        proxy_config.kind,
        proxy_config.host,
        proxy_config.port,
        proxy_config.username.is_some(),
        proxy_config.password.is_some()
    );

    let password_bytes = proxy_config
        .password
        .as_ref()
        .map(|p| p.as_bytes().to_vec());

    let entry = ProxyEntry {
        id: existing_id.unwrap_or(0),
        name: String::new(), // anonymous — tied to the host
        kind: proxy_config.kind,
        host: proxy_config.host.clone(),
        port: proxy_config.port,
        username: proxy_config.username.clone(),
        password: password_bytes,
        created_at: 0,
    };

    let store = store.lock();
    match existing_id {
        Some(id) => {
            let res = store.update_proxy(&entry);
            #[cfg(debug_assertions)]
            tracing::info!(
                "upsert_proxy_for_host: update_proxy({}) result={:?}",
                id,
                res.as_ref().err()
            );
            let _ = res;
            Some(id)
        }
        None => {
            let res = store.add_proxy(&entry);
            #[cfg(debug_assertions)]
            tracing::info!(
                "upsert_proxy_for_host: add_proxy result={:?}",
                res.as_ref().err()
            );
            res.ok()
        }
    }
}

/// Open the main terminal window.
///
/// This is the heavy window — owns the `CrabportApp` root view (tabs,
/// terminals, SFTP, command palette, etc.). Constructed directly here rather
/// than going through `crate::windows::focus_or_open`, because the main
/// window is neither singleton-managed nor lightweight.
///
/// Cross-window sharing still happens via `App`-level globals: `AppState`
/// for the persistent store, `WindowRegistry` for singleton auxiliary
/// windows.
pub fn open_main_window(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1200.0), px(800.0)), cx)),
        #[cfg(target_os = "macos")]
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(14.0))),
            ..Default::default()
        }),
        window_min_size: Some(Size {
            width: px(560.0),
            height: px(340.0),
        }),
        ..Default::default()
    };

    cx.open_window(options, |_window, cx| {
        cx.new(|cx| {
            let app = cx.new(|cx| CrabportApp::new(_window, cx));
            app.update(cx, |app, cx| app.wire(cx));
            gpui_component::Root::new(app, _window, cx)
        })
    })
    .expect("Failed to open main window");
}
