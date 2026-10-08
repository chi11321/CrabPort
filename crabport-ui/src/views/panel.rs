pub mod ai;
pub mod history_command_panel;
pub mod scaffold;
pub mod sftp;
pub mod snippets_panel;
pub mod tunnels_panel;

/// Semantic identity of a right-hand panel pane. Stored on the app as a
/// per-tab `panel_active_tab` map so each terminal connection keeps its own
/// selection and the user's last choice survives switches between terminal
/// backends whose pane sets differ (e.g. SSH shows all four; Telnet shows only
/// History + Snippets). The positional index used by `Tabs` is derived from
/// this at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PanelKind {
    #[default]
    History,
    Snippets,
    Sftp,
    Tunnels,
    /// AI assistant. Endpoint-scoped (not backend-scoped): shown on any
    /// terminal tab while `config.ai.enabled` is true.
    Ai,
}

impl PanelKind {
    /// The page a tab's panel shows until the user picks one there, per the
    /// `appearance.terminal.panel_page` setting. A page the tab doesn't offer
    /// is clamped by [`crate::layouts::panel::render_panel`] to the first one
    /// it does.
    pub fn from_config(page: crabport_core::config::PanelPage) -> Self {
        match page {
            crabport_core::config::PanelPage::Sftp => Self::Sftp,
            crabport_core::config::PanelPage::Tunnels => Self::Tunnels,
            crabport_core::config::PanelPage::History => Self::History,
            crabport_core::config::PanelPage::Snippets => Self::Snippets,
            crabport_core::config::PanelPage::Ai => Self::Ai,
        }
    }
}
