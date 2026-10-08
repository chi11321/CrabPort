//! The agent's tunnel model: a *session-local* definition owned by one
//! conversation, with the id tokens (`a<N>`) the model refers to.

use std::sync::Arc;

use rust_i18n::t;

use crabport_ssh::{TunnelId, TunnelKind, TunnelManager};

/// One tunnel the agent defined through this terminal's connection.
///
/// Agent-facing ids are stable and never reused (a delete keeps its slot),
/// so a `tunnel_open` the model issues later cannot hit the wrong tunnel.
/// The tunnels live in the panel's own [`TunnelManager`] — scoped to this
/// terminal's connection, invisible to every other tab.
#[derive(Clone)]
pub(crate) struct AgentTunnel {
    pub(crate) agent_id: u64,
    pub(crate) kind: TunnelKind,
    pub(crate) name: String,
    pub(crate) bind_addr: String,
    pub(crate) bind_port: u16,
    pub(crate) target_host: String,
    pub(crate) target_port: u16,
    pub(crate) state: AgentTunnelState,
}

/// Lifecycle of an agent-created tunnel.
#[derive(Clone)]
pub(crate) enum AgentTunnelState {
    /// Defined, not started.
    Created,
    /// Started; carries the manager's tunnel id.
    Running(TunnelId),
    /// Stopped — the manager's entry is gone. Can be opened again.
    Closed,
    /// The last open attempt failed, with the reason.
    Failed(String),
    /// Deleted. Kept in the list so earlier ids stay valid.
    Deleted,
}

impl AgentTunnel {
    /// The token the agent refers to this tunnel by — `a<agent_id>`, so it
    /// can never collide with a Tunnels-page config id (`t<db id>`).
    pub(crate) fn handle(&self) -> String {
        format!("a{}", self.agent_id)
    }

    /// The full address pair of this tunnel — `bind -> target` for local and
    /// remote tunnels (the model needs both ends), just the bind port for
    /// dynamic ones (whose target is picked per connection). `0` as a bind
    /// port means "unused / pick a free one at open".
    pub(crate) fn addresses(&self) -> String {
        let mut out = format!("{}:{}", self.bind_addr, self.bind_port);
        if self.kind != TunnelKind::Dynamic && !self.target_host.is_empty() && self.target_port != 0
        {
            out.push_str(&format!(" -> {}:{}", self.target_host, self.target_port));
        }
        out
    }

    /// One-line description for cards and results.
    pub(crate) fn describe(&self, manager: Option<&Arc<TunnelManager>>) -> String {
        let base = format!("{} {} ({})", self.handle(), self.name, self.kind.as_str());
        match &self.state {
            AgentTunnelState::Created => {
                format!("{base} — created, not started ({})", self.addresses())
            }
            AgentTunnelState::Closed => format!("{base} — closed ({})", self.addresses()),
            AgentTunnelState::Deleted => format!("{base} — deleted ({})", self.addresses()),
            AgentTunnelState::Failed(reason) => {
                format!("{base} — open failed: {reason} ({})", self.addresses())
            }
            AgentTunnelState::Running(id) => match manager.and_then(|m| m.get(*id)) {
                Some(info) => {
                    let mut text = format!(
                        "{base} — {}",
                        t!(
                            "ai_panel.tunnel_running_at",
                            bind = format!("{}:{}", info.bind_addr, info.bind_port)
                        )
                    );
                    if !info.target_host.is_empty() && info.target_port != 0 {
                        text.push_str(&format!(" -> {}:{}", info.target_host, info.target_port));
                    }
                    text
                }
                None => format!("{base} — running ({})", self.addresses()),
            },
        }
    }

    /// One labelled row of the `tunnel_list` table: id / name / kind /
    /// address / status. The id token leads — it is what the open / close /
    /// delete calls take.
    pub(crate) fn list_row(&self, manager: Option<&Arc<TunnelManager>>) -> Vec<String> {
        let mut address = self.addresses();
        let status = match &self.state {
            AgentTunnelState::Running(id) => {
                if let Some(info) = manager.and_then(|manager| manager.get(*id)) {
                    address = format!("{}:{}", info.bind_addr, info.bind_port);
                    if !info.target_host.is_empty() && info.target_port != 0 {
                        address.push_str(&format!(" -> {}:{}", info.target_host, info.target_port));
                    }
                }
                t!("ai_panel.tunnel_state_running")
            }
            AgentTunnelState::Created => t!("ai_panel.tunnel_state_created"),
            AgentTunnelState::Closed => t!("ai_panel.tunnel_state_closed"),
            AgentTunnelState::Deleted => t!("ai_panel.tunnel_state_deleted"),
            AgentTunnelState::Failed(_) => t!("ai_panel.tunnel_state_failed"),
        };
        vec![
            self.handle(),
            self.name.clone(),
            self.kind.as_str().to_string(),
            address,
            status.to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `tunnel_list` row: the actionable id token first, kind and
    /// addresses next, and the state word last; a running tunnel reports the
    /// *actual* listen address from the manager when it has one.
    #[test]
    fn list_row_carries_token_kind_address_status() {
        let tunnel = AgentTunnel {
            agent_id: 3,
            kind: TunnelKind::Local,
            name: "db".to_string(),
            bind_addr: "127.0.0.1".to_string(),
            bind_port: 15432,
            target_host: "db.internal".to_string(),
            target_port: 5432,
            state: AgentTunnelState::Created,
        };
        let row = tunnel.list_row(None);
        assert_eq!(row[0], "a3");
        assert_eq!(row[1], "db");
        assert_eq!(row[2], "local");
        assert!(row[3].contains("127.0.0.1:15432"), "{}", row[3]);
        assert!(row[3].contains("db.internal:5432"), "{}", row[3]);
        assert!(!row[4].is_empty());
    }
}
