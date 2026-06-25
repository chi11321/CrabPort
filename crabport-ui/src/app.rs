use std::collections::HashMap;
use std::sync::Arc;

use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::color::*;
use crate::layouts::command_palette::{Command, ConnectionType};
use crate::layouts::connection_form::ConnectionFormState;
use crate::layouts::content::render_content;
use crate::layouts::sidebar::render_sidebar;
use crate::views::hosts::ConnectionHost;
use crate::views::terminal::TerminalView;
use crabport_ssh::SshBackend;
use crabport_ssh::session::SshConnectionInfo;

// ---- CrabPortTab trait ----

pub trait CrabPortTab: 'static {
    /// Release resources before the tab entity is dropped.
    fn close(&mut self);
}

// ---- App ----

actions!(app, [ToggleCommand]);

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
    pub show_command: bool,
    pub hosts: Vec<ConnectionHost>,
    pub connection_form: ConnectionFormState,
    command_search_state: Option<Entity<InputState>>,
    pub form_host_input: Option<Entity<InputState>>,
    pub form_port_input: Option<Entity<InputState>>,
    pub form_user_input: Option<Entity<InputState>>,
    pub form_pass_input: Option<Entity<InputState>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarItem {
    Hosts,
    Credentials,
    Snippets,
    History,
}

impl SidebarItem {
    pub fn label(&self) -> SharedString {
        match self {
            SidebarItem::Hosts => t!("sidebar.hosts").into(),
            SidebarItem::Credentials => t!("sidebar.credentials").into(),
            SidebarItem::Snippets => t!("sidebar.snippets").into(),
            SidebarItem::History => t!("sidebar.history").into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            SidebarItem::Hosts => "icons/server.svg",
            SidebarItem::Credentials => "icons/key.svg",
            SidebarItem::Snippets => "icons/braces.svg",
            SidebarItem::History => "icons/clock.svg",
        }
    }

    pub fn all() -> [SidebarItem; 4] {
        [
            SidebarItem::Hosts,
            SidebarItem::Credentials,
            SidebarItem::Snippets,
            SidebarItem::History,
        ]
    }
}

impl CrabportApp {
    pub fn new() -> Self {
        rust_i18n::set_locale("zh-CN");
        let home_tab = Tab {
            id: 0,
            title: "Home".into(),
            kind: TabKind::Home,
        };
        Self {
            sidebar_item: SidebarItem::Hosts,
            tabs: vec![home_tab],
            active_tab_id: 0,
            hovered_tab_id: None,
            next_tab_id: 1,
            terminal_views: HashMap::new(),
            show_command: false,
            hosts: Vec::new(),
            connection_form: ConnectionFormState::new(),
            command_search_state: None,
            form_host_input: None,
            form_port_input: None,
            form_user_input: None,
            form_pass_input: None,
        }
    }

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
        // Never close the Home tab
        if id == 0 {
            return;
        }

        // Call close() on the view to release resources (e.g. kill PTY)
        if let Some(view) = self.terminal_views.remove(&id) {
            view.update(cx, |v, _cx| {
                v.close();
            });
        }

        self.tabs.retain(|t| t.id != id);

        // If the closed tab was active, switch to Home
        if self.active_tab_id == id {
            self.active_tab_id = 0;
        }
    }

    /// Toggle the command palette open / closed.
    pub fn toggle_command(
        &mut self,
        _: &ToggleCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_command = !self.show_command;
        if !self.show_command {
            self.command_search_state = None;
        }
        cx.notify();
    }
}

impl Render for CrabportApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let show_sidebar = self.is_home_active();

        // Lazy-init the search InputState entity when the command opens.
        if self.show_command && self.command_search_state.is_none() {
            self.command_search_state = Some(cx.new(|cx| InputState::new(_window, cx)));
        }

        // Lazy-init form InputState entities when the form opens.
        if self.connection_form.active {
            if self.form_host_input.is_none() {
                self.form_host_input = Some(cx.new(|cx| InputState::new(_window, cx)));
                // Auto-focus host field when form opens
                if let Some(ref host) = self.form_host_input {
                    host.update(cx, |state, cx| {
                        state.focus(_window, cx);
                    });
                }
            }
            if self.form_port_input.is_none() {
                self.form_port_input = Some(cx.new(|cx| InputState::new(_window, cx)));
                // Pre-fill default port
                if let Some(ref port) = self.form_port_input {
                    port.update(cx, |state, cx| {
                        state.set_value("22", _window, cx);
                    });
                }
            }
            if self.form_user_input.is_none() {
                self.form_user_input = Some(cx.new(|cx| InputState::new(_window, cx)));
            }
            if self.form_pass_input.is_none() {
                self.form_pass_input = Some(cx.new(|cx| {
                    let mut state = InputState::new(_window, cx);
                    state.set_masked(true, _window, cx);
                    state
                }));
            }
        } else {
            // Reset inputs when form closes
            self.form_host_input = None;
            self.form_port_input = None;
            self.form_user_input = None;
            self.form_pass_input = None;
        }

        // Collect host names (placeholder – wire up real data later).
        let host_names: Vec<String> = Vec::new();

        div()
            .size_full()
            .bg(rgb(BG_BASE))
            .flex()
            .flex_row()
            .key_context("App")
            .on_action(cx.listener(Self::toggle_command))
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
            .child(render_content(
                self.sidebar_item,
                &handle,
                &self.tabs,
                self.active_tab_id,
                &self.terminal_views,
                &self.hosts,
                &self.connection_form,
                &self.form_host_input,
                &self.form_port_input,
                &self.form_user_input,
                &self.form_pass_input,
                _window,
                &*cx,
            ))
            // ---- Command palette (always rendered; open drives transitions) ----
            .child({
                let mut cmd = Command::new()
                    .open(self.show_command)
                    .hosts(host_names)
                    .on_close({
                        let handle = handle.clone();
                        move |_, cx| {
                            handle.update(cx, |app, cx| {
                                app.show_command = false;
                                app.command_search_state = None;
                                cx.notify();
                            });
                        }
                    })
                    .on_new_connection({
                        let handle = handle.clone();
                        move |ct, _, cx| {
                            handle.update(cx, |app, cx| {
                                match ct {
                                    ConnectionType::LocalTerminal => {
                                        // Directly open a local terminal
                                        app.add_tab(cx);
                                    }
                                    ConnectionType::SSH
                                    | ConnectionType::SFTP
                                    | ConnectionType::Telnet
                                    | ConnectionType::Serial => {
                                        // Switch to Home tab and open the hosts form
                                        app.activate_tab(0);
                                        app.sidebar_item = SidebarItem::Hosts;
                                        app.connection_form.active = true;
                                    }
                                }
                                app.show_command = false;
                                app.command_search_state = None;
                                cx.notify();
                            });
                        }
                    });
                if let Some(ref search_state) = self.command_search_state {
                    cmd = cmd.search_state(search_state.clone());
                }
                cmd
            })
    }
}
