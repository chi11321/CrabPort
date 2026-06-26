use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crate::color::*;
use crate::layouts::command_palette::{CommandView, ConnectionType};
use crate::layouts::connection_form::ConnectionFormView;
use crate::layouts::credential_form::CredentialFormView;
use crate::layouts::sidebar::render_sidebar;
use crate::layouts::tabbar::render_tab_bar;
use crate::views;
use crate::views::hosts::ConnectionHost;
use crate::views::terminal::TerminalView;
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
    pub connection_form: Option<Entity<ConnectionFormView>>,
    pub credential_form: Option<Entity<CredentialFormView>>,
    pub command_palette: Entity<CommandView>,
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
        rust_i18n::set_locale("zh-CN");
        let home_tab = Tab {
            id: 0,
            title: "Home".into(),
            kind: TabKind::Home,
        };

        let command_palette = cx.new(|cx| CommandView::new(window, cx));

        Self {
            sidebar_item: SidebarItem::Hosts,
            tabs: vec![home_tab],
            active_tab_id: 0,
            hovered_tab_id: None,
            next_tab_id: 1,
            terminal_views: HashMap::new(),
            hosts: Vec::new(),
            connection_form: None,
            credential_form: None,
            command_palette,
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
        });
    }

    // -----------------------------------------------------------------------
    // Connection form (ephemeral entity — created on open, destroyed after close animation)
    // -----------------------------------------------------------------------

    /// Create a new ConnectionFormView entity, wire its callbacks, and open it.
    pub fn open_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // If one is already open, just bring it to front
        if let Some(ref form) = self.connection_form {
            form.update(cx, |form, cx| form.open(window, cx));
            cx.notify();
            return;
        }

        let form = cx.new(|cx| ConnectionFormView::new(window, cx));
        let form_for_connect = form.clone();
        let app = cx.entity().clone();

        form.update(cx, |form, _cx| {
            form.set_on_close({
                let a = app.clone();
                move |_, cx| {
                    a.update(cx, |app, cx| {
                        app.close_connection_form(cx);
                    });
                }
            });
            form.set_on_connect({
                let f = form_for_connect.clone();
                let a = app.clone();
                move |_kind, _, cx| {
                    let (host, port_num, username, password) = {
                        let ff = f.read(cx);
                        let h = ff.host_text(cx);
                        let p: u16 = ff.port_text(cx).parse().unwrap_or(22);
                        let u = ff.user_text(cx);
                        let pw = ff.pass_text(cx);
                        (h, p, u, pw)
                    };
                    a.update(cx, |app, cx| {
                        app.close_connection_form(cx);
                        let name = format!("{}@{}", username, host);
                        app.hosts.push(ConnectionHost {
                            name,
                            host: host.to_string(),
                            port: port_num,
                            username: username.to_string(),
                        });
                        app.add_ssh_tab(&host, port_num, &username, &password, cx);
                        cx.notify();
                    });
                }
            });
        });

        form.update(cx, |form, cx| form.open(window, cx));
        self.connection_form = Some(form);
        cx.notify();
    }

    /// Close the connection form. The entity stays alive for the exit animation,
    /// then is destroyed by a timer.
    pub fn close_connection_form(&mut self, cx: &mut Context<Self>) {
        let form = match self.connection_form.as_ref() {
            Some(f) => f.clone(),
            None => return,
        };
        // Trigger the close animation inside the form
        form.update(cx, |form, cx| form.close(cx));
        // After animation finishes, destroy the entity
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                app.connection_form = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Create a new CredentialFormView entity, wire its callbacks, and open it.
    pub fn open_credential_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref form) = self.credential_form {
            form.update(cx, |form, cx| form.open(window, cx));
            cx.notify();
            return;
        }

        let form = cx.new(|cx| CredentialFormView::new(window, cx));
        let form_clone = form.clone();
        let app = cx.entity().clone();

        form.update(cx, |form, _cx| {
            form.set_on_close({
                let app = app.clone();
                move |_w, cx| {
                    app.update(cx, |app, cx| {
                        app.close_credential_form(cx);
                    });
                }
            });

            form.set_on_kind_change({
                let form = form_clone.clone();
                move |kind, _w, cx| {
                    form.update(cx, |f, cx| {
                        f.kind = kind;
                        cx.notify();
                    });
                }
            });

            form.set_on_save({
                let app = app.clone();
                move |_kind, _w, cx| {
                    app.update(cx, |app, cx| {
                        app.close_credential_form(cx);
                        cx.notify();
                    });
                }
            });
        });

        form.update(cx, |form, cx| form.open(window, cx));
        self.credential_form = Some(form);
        cx.notify();
    }

    /// Close the credential form. The entity stays alive for the exit animation,
    /// then is destroyed by a timer.
    pub fn close_credential_form(&mut self, cx: &mut Context<Self>) {
        let form = match self.credential_form.as_ref() {
            Some(f) => f.clone(),
            None => return,
        };
        form.update(cx, |form, cx| form.close(cx));
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                app.credential_form = None;
                cx.notify();
            });
        })
        .detach();
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
        });

        let terminal_view = cx.new(|cx| TerminalView::new(cx));
        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn add_ssh_tab(
        &mut self,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = format!("{}@{}", username, host);
        self.tabs.push(Tab {
            id,
            title,
            kind: TabKind::Terminal,
        });

        let info = SshConnectionInfo::new(host, username, password).with_port(port);
        let cols: usize = 80;
        let rows: usize = 24;
        let backend = Arc::new(SshBackend::new(info, cols as u16, rows as u16));
        let terminal_view = cx.new(|cx| TerminalView::with_backend(backend, cols, rows, cx));
        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn activate_tab(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_tab_id = id;
        }
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

    fn close_tab_fn(handle: Entity<Self>) -> Rc<dyn Fn(u64, &mut Window, &mut App)> {
        Rc::new(move |id, _, cx| {
            handle.update(cx, |app, cx| app.close_tab(id, cx));
        })
    }
}

impl Render for CrabportApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let show_sidebar = self.is_home_active();
        let active_tab = self.tabs.iter().find(|t| t.id == self.active_tab_id);
        let is_home_tab = active_tab.map(|t| t.kind == TabKind::Home).unwrap_or(false);

        // Host names for command palette
        let host_names: Vec<String> = self.hosts.iter().map(|h| h.name.clone()).collect();
        self.command_palette.update(cx, |cmd, _cx| {
            cmd.set_hosts(host_names);
        });

        // ---- Content view (inlined from render_content) ----
        let app_handle = cx.entity().clone();
        let on_new = move |w: &mut Window, cx: &mut App| {
            app_handle.update(cx, |app, cx| {
                app.open_connection_form(w, cx);
            });
        };

        let content_view: AnyElement = match active_tab.map(|t| t.kind) {
            Some(TabKind::Home) => match self.sidebar_item {
                SidebarItem::Hosts => views::hosts::render_hosts_view(
                    &self.hosts,
                    self.connection_form.as_ref(),
                    on_new,
                )
                .into_any_element(),
                SidebarItem::Credentials => {
                    let on_new_cred = {
                        let app = cx.entity().clone();
                        move |_w: &mut Window, cx: &mut App| {
                            app.update(cx, |app, cx| {
                                app.open_credential_form(_w, cx);
                            });
                        }
                    };
                    views::credentials::render_credentials_view(
                        self.credential_form.as_ref(),
                        on_new_cred,
                    )
                    .into_any_element()
                }
                SidebarItem::Tunnels => {
                    views::tunnels::render_tunnels_view(on_new).into_any_element()
                }
                SidebarItem::Snippets => views::snippets::render_snippets_view().into_any_element(),
                SidebarItem::History => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(rgb(TEXT_MUTED))
                            .child(self.sidebar_item.label()),
                    )
                    .into_any_element(),
            },
            Some(TabKind::Terminal) => {
                if let Some(terminal_entity) =
                    active_tab.and_then(|tab| self.terminal_views.get(&tab.id))
                {
                    terminal_entity.read_with(cx, |view, cx| {
                        _window.focus(&view.focus_handle(cx));
                    });
                    div()
                        .size_full()
                        .m_2()
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
                    .transition_when(
                        show_sidebar,
                        std::time::Duration::from_millis(300),
                        gpui_animation::transition::general::EaseInOutCubic,
                        |el| el.w(px(200.0)),
                    )
                    .transition_when(
                        !show_sidebar,
                        std::time::Duration::from_millis(300),
                        gpui_animation::transition::general::EaseInOutCubic,
                        |el| el.w_0(),
                    )
                    .child(render_sidebar(self.sidebar_item, &handle)),
            )
            // -- Tab bar + content --
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .bg(rgb(BG_BASE))
                    .flex()
                    .flex_col()
                    .child(render_tab_bar(
                        &handle,
                        &self.tabs,
                        self.active_tab_id,
                        is_home_tab,
                        Self::close_tab_fn(handle.clone()),
                    ))
                    .child(content_view),
            )
            // -- Command palette --
            .child(self.command_palette.clone())
    }
}
