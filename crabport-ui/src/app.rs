use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use rust_i18n::t;

use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::AlertController;
use crate::layouts::command_palette::{CommandView, ConnectionType};
use crate::layouts::connection_form::{AuthKind, ConnectionFormState, ConnectionKind};
use crate::layouts::sidebar::render_sidebar;
use crate::views::hosts::ConnectionHost;
use crate::views::terminal::TerminalView;
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
    ProxyConfig, ProxyEntry,
};
use crabport_ssh::backend::SshBackend;
use crabport_ssh::session::SshConnectionInfo;

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
    /// Active index of the right-hand panel's tab strip (SFTP / History /
    /// Snippets). Driven by `Tabs::on_change` in `render_panel`.
    pub panel_active_tab: usize,
    pub hosts_view: Entity<crate::views::hosts::HostsView>,
    /// Snippets management sidebar view (right-click edit/delete).
    pub snippets_view: Entity<crate::views::snippets::SnippetsView>,
    /// Global alert dialog host. Rendered at the app root so it overlays the
    /// entire window regardless of which view is active. Triggered via
    /// `alert_controller.update(cx, |c, cx| c.show(state, cx))`.
    pub alert_controller: Entity<AlertController>,
    /// Global context menu host. Triggered via
    /// `context_menu.update(cx, |c, cx| c.show(state, cx))`.
    pub context_menu: Entity<ContextMenuController>,
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
        let app_entity = cx.entity();
        let hosts_view = cx.new(|_cx| crate::views::hosts::HostsView::new(app_entity));
        let snippets_view = cx.new(|_cx| crate::views::snippets::SnippetsView::new());
        let alert_controller = cx.new(|_cx| AlertController::new());
        let context_menu = cx.new(|_cx| ContextMenuController::new());

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
                    CoreHostKind::Ssh => crate::layouts::connection_form::ConnectionKind::SSH,
                    CoreHostKind::Telnet => crate::layouts::connection_form::ConnectionKind::Telnet,
                    CoreHostKind::Serial => crate::layouts::connection_form::ConnectionKind::Serial,
                },
                credential_id: h.credential_id,
                last_login: h.last_login,
                favorite: h.favorite,
                proxy_id: h.proxy_id,
            })
            .collect();

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
            panel_active_tab: 0,
            hosts_view,
            snippets_view,
            alert_controller,
            context_menu,
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
                        kind: crate::layouts::connection_form::ConnectionKind::SSH,
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
                        CoreHostKind::Ssh => crate::layouts::connection_form::ConnectionKind::SSH,
                        CoreHostKind::Telnet => {
                            crate::layouts::connection_form::ConnectionKind::Telnet
                        }
                        CoreHostKind::Serial => {
                            crate::layouts::connection_form::ConnectionKind::Serial
                        }
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
            self.panel_active_tab,
            &self.hosts_view,
            &self.snippets_view,
            &self.alert_controller,
            &self.context_menu,
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
            width: px(480.0),
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
