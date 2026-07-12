// Submodules — method groups split out of this file. Each file holds an
// `impl CrabportApp { ... }` block; the methods are reachable on
// `CrabportApp` because all `impl` blocks for the same type compose.
pub mod connection;
pub mod context;
pub mod groups;
pub mod hosts;
pub mod snippets;
pub mod tabs;
pub mod tunnels;

pub use context::AppCtx;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use rust_i18n::t;

use crate::app_state::AppState;
use crate::color::*;
use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::AlertController;
use crate::components::notification::{NotificationController, NotificationPosition};
use crate::layouts::command_palette::{CommandView, ConnectionType};
use crate::layouts::panel::PanelDrag;
use crate::layouts::sidebar::render_sidebar;
use crate::views::groups::GroupFormState;
use crate::views::sessions::ConnectionFormState;
use crate::views::sessions::ConnectionHost;
use crate::views::sftp::SftpTabView;
use crate::views::terminal::TerminalView;
use crabport_core::credential::HostKind as CoreHostKind;

// ---- CrabPortTab trait ----

pub trait CrabPortTab: 'static {
    fn close(&mut self);
}

// ---- App ----

actions!(
    app,
    [
        ToggleCommand,
        TerminalTab,
        TerminalShiftTab,
        TerminalIncreaseFont,
        TerminalDecreaseFont,
        TerminalResetFont
    ]
);

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
    Sftp,
}

pub struct CrabportApp {
    pub sidebar_item: SidebarItem,
    pub tabs: Vec<Tab>,
    pub active_tab_id: u64,
    pub hovered_tab_id: Option<u64>,
    pub next_tab_id: u64,
    pub terminal_views: HashMap<u64, Entity<TerminalView>>,
    /// Single persistent SFTP tab view (id=1). Both left and right panels
    /// can independently connect to local or remote hosts.
    pub sftp_view: Entity<SftpTabView>,
    /// Per-tab split layout. Each terminal tab owns a [`SplitTree`] describing
    /// how its panes are arranged. Absent for non-terminal tabs and terminal
    /// tabs that haven't been split (a single pane is still tracked here so
    /// close/active-pane logic is uniform).
    pub split_trees: HashMap<u64, crate::views::terminal::split::SplitTree>,
    /// Every terminal pane's view, keyed by pane id (NOT tab id — a split tab
    /// has multiple panes). `terminal_views` (above) is kept in sync to point
    /// at the *active* pane's view so the existing toolbar / panel / SFTP /
    /// tunnel-borrow logic in `content.rs` keeps working without per-pane
    /// lookups.
    pub pane_views: HashMap<u64, Entity<TerminalView>>,
    /// Monotonic pane-id counter, so pane ids are unique across the whole app
    /// (avoids id collisions in the gpui element-id space when a pane is
    /// moved between tabs in the future).
    pub next_pane_id: u64,
    /// The last terminal pane that had keyboard focus, tracked via each
    /// pane's `on_focused` callback. Unlike per-pane `is_focused`, this is
    /// **not** cleared when focus moves to a non-terminal element (e.g. the
    /// split-toolbar button), so `split_active_pane` can always split the
    /// pane the user was last typing in. Cleared/updated only when another
    /// terminal pane gains focus, or when the pane is closed.
    pub last_focused_pane: Option<u64>,
    /// Pane id to focus on the next render, set by `split_active_pane` so the
    /// newly-created pane receives keyboard focus (and its cursor becomes
    /// solid). Consumed in `render`.
    pub pending_focus_pane: Option<u64>,
    /// Active divider drag, if any. Set when the user presses on a split
    /// divider; the split container records its pixel extent so each
    /// mouse-move can convert the cursor position into a ratio.
    pub split_drag: Option<crate::views::terminal::split::SplitDrag>,
    pub hosts: Vec<ConnectionHost>,
    pub connection_form: Option<ConnectionFormState>,
    /// Which right-hand panel pane the user last selected, keyed by tab id
    /// so each terminal connection keeps its own panel selection (e.g. one
    /// tab can show SFTP while another shows Tunnels). Stored as a semantic
    /// [`PanelKind`] (not a positional index) so the selection survives
    /// switches between terminal backends whose pane sets differ (e.g. SSH
    /// shows all four; Telnet shows only History + Snippets). Lookups fall
    /// back to the default [`PanelKind`] for tabs that haven't been visited.
    pub panel_active_tab: HashMap<u64, crate::views::panel::PanelKind>,
    /// Live panel resize drag state. When `Some`, the panel width tracks the
    /// cursor. On mouse-up the final width is persisted to config and this
    /// is cleared.
    pub panel_drag: Option<PanelDrag>,
    /// Tunnel form window state (singleton dialog for creating/editing a
    /// tunnel config). `None` when the dialog is closed.
    pub tunnel_form: Option<crate::views::tunnels::TunnelFormState>,
    /// Snippet form window state (singleton dialog for creating/editing a
    /// snippet). `None` when the dialog is closed.
    pub snippet_form: Option<crate::views::snippets::SnippetFormState>,
    /// Group form window state (singleton dialog for creating / renaming a
    /// group). Shared across all collection kinds (Host / Snippet / Tunnel);
    /// `None` when the dialog is closed.
    pub group_form: Option<GroupFormState>,
    /// Single entry point for all long-lived shared services: global overlay
    /// controllers (alert / context-menu / notifications), the tunnel
    /// registry, the command palette, and the side-panel + sidebar views.
    /// Child modules reach them via `self.app_ctx.<field>`.
    pub app_ctx: AppCtx,
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
        let sftp_tab = Tab {
            id: 1,
            title: "SFTP".into(),
            kind: TabKind::Sftp,
            is_remote: false,
        };

        // Create the persistent SftpTabView (both panels start as Local).
        let sftp_view = cx.new(|_cx| SftpTabView::new());

        // ---- Construct shared entities (all live in `AppCtx`) ----
        let command_palette = cx.new(|cx| CommandView::new(window, cx));
        let sftp_panel = cx.new(|_cx| crate::views::panel::sftp::SftpPanel::new());
        let snippets_panel =
            cx.new(|_cx| crate::views::panel::snippets_panel::SnippetsPanel::new());
        let history_panel =
            cx.new(|_cx| crate::views::panel::history_command_panel::HistoryCommandPanel::new());
        let tunnels_panel = cx.new(|_cx| crate::views::panel::tunnels_panel::TunnelsPanel::new());
        let app_entity = cx.entity();
        let sessions_view =
            cx.new(|_cx| crate::views::sessions::SessionsView::new(app_entity.clone()));
        let snippets_view =
            cx.new(|_cx| crate::views::snippets::SnippetsView::new(app_entity.clone()));
        let tunnels_view =
            cx.new(|_cx| crate::views::tunnels::TunnelsView::new(app_entity.clone()));
        let alert = cx.new(|_cx| AlertController::new());
        let context_menu = cx.new(|_cx| ContextMenuController::new());
        let tooltip = cx.new(|_cx| crate::components::tooltip::TooltipController::new());
        let notifications =
            cx.new(|_cx| NotificationController::new(NotificationPosition::BottomRight));
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
                    CoreHostKind::Ssh => crate::views::sessions::ConnectionKind::SSH,
                    CoreHostKind::Telnet => crate::views::sessions::ConnectionKind::Telnet,
                    CoreHostKind::Serial => crate::views::sessions::ConnectionKind::Serial,
                },
                credential_id: h.credential_id,
                last_login: h.last_login,
                favorite: h.favorite,
                proxy_id: h.proxy_id,
                group_id: h.group_id,
            })
            .collect();

        // Load persisted tunnel configs from the store. Tunnels start in the
        // stopped state — the user starts them explicitly from the Tunnels
        // view or a terminal panel.
        let tunnel_configs = store.lock().tunnels().unwrap_or_default();
        tunnels.set_configs(tunnel_configs);

        // Shared context bundle: the single home for every long-lived service.
        // Built after `tunnels` is fully initialized so the bundle wraps the
        // final registry.
        let app_ctx = AppCtx {
            app: app_entity,
            alert,
            context_menu,
            tooltip,
            notifications,
            tunnels,
            command_palette,
            sftp_panel,
            snippets_panel,
            history_panel,
            tunnels_panel,
            sessions_view,
            snippets_view,
            tunnels_view,
        };

        Self {
            sidebar_item: SidebarItem::Sessions,
            tabs: vec![home_tab, sftp_tab],
            active_tab_id: 0,
            hovered_tab_id: None,
            next_tab_id: 2,
            terminal_views: HashMap::new(),
            sftp_view,
            split_trees: HashMap::new(),
            pane_views: HashMap::new(),
            next_pane_id: 1,
            last_focused_pane: None,
            pending_focus_pane: None,
            split_drag: None,
            hosts,
            connection_form: None,
            panel_active_tab: HashMap::new(),
            panel_drag: None,
            tunnel_form: None,
            snippet_form: None,
            group_form: None,
            app_ctx,
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

        let cmd = self.app_ctx.command_palette.clone();
        let app = cx.entity().clone();

        // Hand the app entity to the tunnels panel so its per-row star toggle
        // can drive favorite toggles without a new `set_state` param (which
        // would require touching `content.rs`).
        self.app_ctx
            .tunnels_panel
            .update(cx, |p, _cx| p.set_app(app.clone()));

        // ---- Command palette callbacks ----
        let cmd_for_close = cmd.clone();
        let cmd_for_new = cmd.clone();
        let app_for_cmd = app.clone();
        self.app_ctx.command_palette.update(cx, move |cmd, _cx| {
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
                        ConnectionType::SFTP => {
                            a.update(cx, |app, cx| {
                                app.activate_tab(1);
                                cx.notify();
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
        self.app_ctx.command_palette.update(cx, |cmd, _cx| {
            cmd.set_hosts(sorted_hosts);
        });

        // ---- Content view ----
        // Pre-read tunnel state here (in the render method, where `self` is
        // already borrowed) rather than via `handle.read_with` inside
        // `render_content` — that would be a nested read of `CrabportApp`
        // and panic ("cannot read while it is already being updated").
        // Resolve the active tab's per-tab panel selection here too, so the
        // panel reflects each terminal connection's own choice.
        let tunnel_list = self.app_ctx.tunnels.list();
        let tunnel_form_state = self.tunnel_form.clone();
        let snippet_form_state = self.snippet_form.clone();
        let group_form_state = self.group_form.clone();
        let panel_active_tab = self
            .panel_active_tab
            .get(&self.active_tab_id)
            .copied()
            .unwrap_or_default();
        // Panel width: live drag value takes priority; otherwise read the
        // persisted config value (clamped to a sane range). The max is also
        // bounded by 2/3 of the window width so the terminal stays usable.
        let win_w = f32::from(_window.viewport_size().width);
        let eff_max = crate::layouts::panel::effective_max_panel_width(win_w);
        let panel_width = self.panel_drag.map_or_else(
            || {
                crabport_core::config::snapshot()
                    .appearance
                    .panel_width
                    .clamp(crate::layouts::panel::MIN_PANEL_WIDTH, eff_max)
            },
            |drag| drag.width,
        );
        let panel_dragging = self.panel_drag.is_some();

        let content = crate::layouts::content::render_content(
            self.sidebar_item,
            &handle,
            &self.tabs,
            self.active_tab_id,
            &self.terminal_views,
            &self.split_trees,
            &self.pane_views,
            &self.sftp_view,
            &self.hosts,
            self.connection_form.as_ref(),
            panel_active_tab,
            tunnel_list,
            tunnel_form_state,
            snippet_form_state,
            panel_width,
            panel_dragging,
            &self.app_ctx,
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

        // Move keyboard focus to a freshly-split pane (set by
        // `split_active_pane`) so the user can immediately type into the new
        // pane and its cursor renders solid.
        if let Some(pane_id) = self.pending_focus_pane.take() {
            if let Some(view) = self.pane_views.get(&pane_id).cloned() {
                let fh = view.read_with(cx, |view, cx| view.focus_handle(cx));
                _window.focus(&fh);
            }
        }

        // ---- Root ----
        div()
            .size_full()
            .bg(rgb(bg_base()))
            .flex()
            .flex_row()
            .key_context("App")
            .on_action(cx.listener(Self::toggle_command))
            // -- Sidebar --
            .child(render_sidebar(self.sidebar_item, show_sidebar, &handle))
            .child(content)
            // -- Command palette --
            .child(self.app_ctx.command_palette.clone())
            // -- Global alert dialog (host-key prompts, etc.) --
            .child(self.app_ctx.alert.clone())
            // -- Global context menu --
            .child(self.app_ctx.context_menu.clone())
            // -- Global tooltip --
            .child(self.app_ctx.tooltip.clone())
            // -- Global toast notifications --
            .child(self.app_ctx.notifications.clone())
            // -- Group form overlay (new / rename group, shared across kinds) --
            .when_some(group_form_state, |el, state| {
                el.child(crate::views::groups::GroupFormView::new(
                    &state,
                    handle.clone(),
                ))
            })
    }
}

// ---------------------------------------------------------------------------
// Main window construction
// ---------------------------------------------------------------------------

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

            // Quit the application when this main window is closed.
            // `cx.on_release` fires for this specific window, not all windows.
            cx.on_release(|_, cx| {
                cx.quit();
            })
            .detach();

            gpui_component::Root::new(app, _window, cx)
        })
    })
    .expect("Failed to open main window");
}
