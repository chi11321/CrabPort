//! Shared app context for cross-module access.
//!
//! [`AppCtx`] is the single entry point for all long-lived shared services:
//! the owning `CrabportApp` entity, the global overlay controllers (alert /
//! context-menu / notifications), the tunnel registry, and the shared
//! side-panel / sidebar views. Child modules and layout functions read from
//! a cloned `AppCtx` instead of receiving a long parameter list, which keeps
//! `app.rs` thin.
//!
//! Construction happens once in [`crate::app::CrabportApp::new`]; the context
//! is stored on the app as `pub app_ctx: AppCtx` and cloned freely (every
//! field is either an `Entity` or `Arc`, so clones are cheap).
//!
//! Named `AppCtx` (not `AppContext`) to avoid colliding with GPUI's
//! `AppContext` trait, which provides `cx.new(...)`.

use std::sync::Arc;

use gpui::*;

use crate::components::context_menu::ContextMenuController;
use crate::components::dialog::AlertController;
use crate::components::notification::NotificationController;
use crate::components::tooltip::TooltipController;
use crate::layouts::command_palette::CommandView;
use crate::views::panel::history_command_panel::HistoryCommandPanel;
use crate::views::panel::sftp::SftpPanel;
use crate::views::panel::snippets_panel::SnippetsPanel;
use crate::views::panel::tunnels_panel::TunnelsPanel;
use crate::views::sessions::SessionsView;
use crate::views::snippets::SnippetsView;
use crate::views::tunnels::{TunnelRegistry, TunnelsView};

/// Shared, cheaply-clonable bundle of every long-lived service the app owns.
///
/// Every field is a GPUI `Entity` (refcounted handle) or an `Arc`, so cloning
/// this struct is essentially free and safe to hand out to child views /
/// layout functions / async tasks.
#[derive(Clone)]
pub struct AppCtx {
    /// Owning `CrabportApp` entity. Use this to drive app-level mutations
    /// (`app.update(cx, |app, cx| ... )`) from outside the app module.
    pub app: Entity<crate::app::CrabportApp>,

    // -- Global overlay controllers (rendered at app root) --
    /// Global alert dialog host (host-key prompts, delete confirmations).
    pub alert: Entity<AlertController>,
    /// Global right-click context-menu host.
    pub context_menu: Entity<ContextMenuController>,
    /// Global tooltip host (hover tooltips with fade-in/out).
    pub tooltip: Entity<TooltipController>,
    /// Global toast notification host.
    pub notifications: Entity<NotificationController>,
    /// Global SFTP transfer-history popover host. Toggled from the SFTP
    /// toolbar's history button; records are pushed by
    /// `SftpTabView::on_sftp_transfer_finished`.
    pub transfer_history: Entity<crate::views::sftp::TransferHistoryController>,

    // -- Shared data registries --
    /// Central tunnel registry (stopped + running). Single source of truth
    /// for the Tunnels view and panel.
    pub tunnels: Arc<TunnelRegistry>,

    // -- Command palette (overlay rendered at app root) --
    pub command_palette: Entity<CommandView>,

    // -- Right-hand side panels (rendered next to the active terminal) --
    pub sftp_panel: Entity<SftpPanel>,
    pub snippets_panel: Entity<SnippetsPanel>,
    pub history_panel: Entity<HistoryCommandPanel>,
    pub tunnels_panel: Entity<TunnelsPanel>,

    // -- Full-page sidebar views (rendered on the Home tab) --
    pub sessions_view: Entity<SessionsView>,
    pub snippets_view: Entity<SnippetsView>,
    pub tunnels_view: Entity<TunnelsView>,
}

impl AppCtx {
    /// Convenience accessor for the notification host, mirroring the helper
    /// methods on `CrabportApp` so call sites can stay terse.
    pub fn notify(
        &self,
        notification: crate::components::notification::Notification,
        cx: &mut App,
    ) {
        self.notifications
            .update(cx, |c, cx| c.show(notification, cx));
    }
}
