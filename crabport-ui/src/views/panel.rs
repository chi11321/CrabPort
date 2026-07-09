pub mod history_command_panel;
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
}
