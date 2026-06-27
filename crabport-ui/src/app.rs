use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crate::color::*;
use crate::layouts::command_palette::{CommandView, ConnectionType};
use crate::layouts::connection_form::{ConnectionFormState, ConnectionKind};
use crate::layouts::credential_form::{CredentialFormState, CredentialKind};
use crate::layouts::sidebar::render_sidebar;
use crate::views::hosts::ConnectionHost;
use crate::views::terminal::TerminalView;
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
};
use crabport_core::store::Store;
use crabport_ssh::SshBackend;
use crabport_ssh::session::SshConnectionInfo;

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
    pub credentials: Vec<CredentialEntry>,
    pub connection_form: Option<ConnectionFormState>,
    pub credential_form: Option<CredentialFormState>,
    pub command_palette: Entity<CommandView>,
    store: Store,
    wired: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarItem {
    Hosts,
    Tunnels,
    Credentials,
    Snippets,
    History,
}

impl SidebarItem {
    pub fn label(&self) -> SharedString {
        match self {
            SidebarItem::Hosts => t!("sidebar.hosts").into(),
            SidebarItem::Credentials => t!("sidebar.credentials").into(),
            SidebarItem::Tunnels => t!("sidebar.tunnels").into(),
            SidebarItem::Snippets => t!("sidebar.snippets").into(),
            SidebarItem::History => t!("sidebar.history").into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            SidebarItem::Hosts => "icons/server.svg",
            SidebarItem::Credentials => "icons/key.svg",
            SidebarItem::Tunnels => "icons/waypoints.svg",
            SidebarItem::Snippets => "icons/braces.svg",
            SidebarItem::History => "icons/clock.svg",
        }
    }

    pub fn all() -> [SidebarItem; 5] {
        [
            SidebarItem::Hosts,
            SidebarItem::Credentials,
            SidebarItem::Tunnels,
            SidebarItem::Snippets,
            SidebarItem::History,
        ]
    }
}

impl CrabportApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        rust_i18n::set_locale("en");
        let home_tab = Tab {
            id: 0,
            title: "Home".into(),
            kind: TabKind::Home,
            is_remote: false,
        };

        let command_palette = cx.new(|cx| CommandView::new(window, cx));

        // Open store and load persisted data
        let store = Store::open().expect("failed to open store");
        let hosts: Vec<ConnectionHost> = store
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
            })
            .collect();
        let credentials = store.credentials().unwrap_or_default();

        Self {
            sidebar_item: SidebarItem::Hosts,
            tabs: vec![home_tab],
            active_tab_id: 0,
            hovered_tab_id: None,
            next_tab_id: 1,
            terminal_views: HashMap::new(),
            hosts,
            credentials,
            connection_form: None,
            credential_form: None,
            command_palette,
            store,
            wired: false,
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
                                app.sidebar_item = SidebarItem::Hosts;
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
                    let (name, host, port_num, username, password) = {
                        let f = app.connection_form.as_ref().unwrap();
                        let n = f.name_text(cx);
                        let h = f.host_text(cx);
                        let p: u16 = f.port_text(cx).parse().unwrap_or(22);
                        let u = f.user_text(cx);
                        let pw = f.pass_text(cx);
                        (n, h, p, u, pw)
                    };
                    app.close_connection_form(cx);

                    // Persist anonymous credential (password) for this host
                    let cred = CredentialEntry {
                        id: 0,
                        name: name.clone(),
                        kind: CoreCredentialKind::Password,
                        anonymous: true,
                        secret: password.clone(),
                        private_key: String::new(),
                        public_key: String::new(),
                        certificate: String::new(),
                    };
                    let cred_id = app.store.add_credential(&cred).unwrap_or(0);

                    // Persist host with linked credential
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
                    };
                    let row_id = app.store.add_host(&entry).unwrap_or(0);

                    // Keep credentials list in sync
                    let mut saved_cred = cred.clone();
                    saved_cred.id = cred_id;
                    app.credentials.push(saved_cred);

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
                    });
                    app.add_ssh_tab(&name, &host, port_num, &username, &password, cx);
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
        // After animation finishes, destroy the state
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                app.connection_form = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Create a new CredentialFormState, wire its callbacks, and open it.
    pub fn open_credential_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.credential_form {
            form.open(window, cx);
            cx.notify();
            return;
        }

        let mut form = CredentialFormState::new(window, cx);
        let app = cx.entity().clone();

        form.on_close = Some(Rc::new({
            let app = app.clone();
            move |_w: &mut Window, cx: &mut App| {
                app.update(cx, |app, cx| {
                    app.close_credential_form(cx);
                });
            }
        }));

        form.on_kind_change = Some(Rc::new({
            let app = app.clone();
            move |kind: CredentialKind, _w: &mut Window, cx: &mut App| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut f) = app.credential_form {
                        f.kind = kind;
                    }
                    cx.notify();
                });
            }
        }));

        form.on_save = Some(Rc::new({
            let a = app.clone();
            move |kind: CredentialKind, _w: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    // Read form values directly from state
                    let (name, secret, private_key, public_key, certificate) = {
                        let f = app.credential_form.as_ref().unwrap();
                        (
                            f.name_text(cx),
                            f.secret_text(cx),
                            f.private_key_text(cx),
                            f.public_key_text(cx),
                            f.certificate_text(cx),
                        )
                    };

                    // Persist to store
                    let core_kind = match kind {
                        crate::layouts::credential_form::CredentialKind::Password => {
                            CoreCredentialKind::Password
                        }
                        crate::layouts::credential_form::CredentialKind::Certificate => {
                            CoreCredentialKind::Certificate
                        }
                    };
                    let entry = CredentialEntry {
                        id: 0, // auto-generated
                        name: name.clone(),
                        kind: core_kind,
                        anonymous: false,
                        secret,
                        private_key,
                        public_key,
                        certificate,
                    };
                    let row_id = app.store.add_credential(&entry).unwrap_or(0);
                    let mut saved = entry.clone();
                    saved.id = row_id;
                    app.credentials.push(saved);

                    app.close_credential_form(cx);
                    cx.notify();
                });
            }
        }));

        form.open(window, cx);
        self.credential_form = Some(form);
        cx.notify();
    }

    /// Close the credential form. The state stays alive for the exit animation,
    /// then is destroyed by a timer.
    pub fn close_credential_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.credential_form {
            form.close();
        } else {
            return;
        }
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                app.credential_form = None;
                cx.notify();
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

        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn add_ssh_tab(
        &mut self,
        name: &str,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
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

        let info = SshConnectionInfo::new(host, username, password).with_port(port);
        let info_for_view = info.clone();
        let cols: usize = 80;
        let rows: usize = 24;

        // Create the overlay state early so the SSH backend callback can write to it
        let overlay: crate::views::terminal::connection_overlay::SharedOverlayState =
            std::sync::Arc::new(parking_lot::Mutex::new(
                crate::views::terminal::connection_overlay::ConnectionOverlayState::new(),
            ));
        let overlay_cb = overlay.clone();

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
        ));
        let terminal_view = cx.new(|cx| {
            TerminalView::with_backend_and_host_and_overlay(
                backend,
                cols,
                rows,
                format!("{}@{}", username, host),
                overlay,
                Some(info_for_view),
                id,
                cx,
            )
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
        let _ = self.store.touch_host_login(host_id);
        if let Ok(all) = self.store.hosts() {
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
                })
                .collect();
        }

        // Try to resolve password from linked credential
        let password = host
            .credential_id
            .and_then(|cid| self.store.find_credential(cid).ok().flatten())
            .map(|c| c.secret)
            .unwrap_or_default();

        self.add_ssh_tab(
            &host.name,
            &host.host,
            host.port,
            &host.username,
            &password,
            cx,
        );
    }

    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if id == 0 {
            return;
        }
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
            &self.credentials,
            self.connection_form.as_ref(),
            self.credential_form.as_ref(),
            _window,
            cx,
        );

        // ---- Root ----
        div()
            .size_full()
            .bg(rgb(BG_BASE))
            .flex()
            .flex_row()
            .key_context("App")
            .on_action(cx.listener(Self::toggle_command))
            // -- Sidebar --
            .child(
                div()
                    .id("sidebar-container")
                    .h_full()
                    .bg(rgb(BG_SIDEBAR))
                    .overflow_x_hidden()
                    .with_transition("sidebar-container")
                    .transition_when_else(
                        show_sidebar,
                        std::time::Duration::from_millis(300),
                        gpui_animation::transition::general::EaseInOutCubic,
                        |el| el.w(px(200.0)),
                        |el| el.w_0(),
                    )
                    .child(render_sidebar(self.sidebar_item, &handle)),
            )
            .child(content)
            // -- Command palette --
            .child(self.command_palette.clone())
    }
}
