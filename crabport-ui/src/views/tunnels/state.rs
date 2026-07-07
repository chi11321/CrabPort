//! Tunnel runtime state + management.
//!
//! Tunnels are persistent configs (`crabport_core::credential::TunnelEntry`)
//! bound to a host. At runtime a tunnel can be in one of three states:
//!
//! - **Stopped** (`None`): no live session.
//! - **Owned**: started from the Tunnels page → a dedicated `OwnedSession`
//!   SSH connection holds the tunnel. Stopped from the Tunnels page.
//! - **Borrowed**: started from a terminal panel → reuses that tab's
//!   `SshBackend` connection. Closed automatically when the tab's SSH
//!   connection closes; can also be stopped from the Tunnels page.
//!
//! Mutual exclusion: a tunnel can be started in only one place at a time.
//! The `start_*` methods reject a start when the tunnel is already running.

use std::sync::Arc;

use parking_lot::Mutex as PlMutex;

use crabport_core::credential::TunnelEntry;
use crabport_ssh::{CrabPortTunnel, OwnedSession, TunnelManager};

/// A tunnel config plus its optional live runtime state.
pub struct TunnelState {
    /// The persisted config (id, name, host_id, kind, ports, ...).
    pub config: TunnelEntry,
    /// `None` when stopped. `Some` when running.
    pub runtime: Option<RuntimeKind>,
}

/// How a running tunnel is backed.
pub enum RuntimeKind {
    /// Started from the Tunnels page — owns its SSH connection.
    Owned {
        session: Arc<OwnedSession>,
        manager: Arc<TunnelManager>,
    },
    /// Started from a terminal panel — borrows that tab's SSH connection.
    /// `tab_id` identifies which tab lent its session, so that closing the
    /// tab can tear down the borrowed tunnel.
    Borrowed {
        tab_id: u64,
        manager: Arc<TunnelManager>,
    },
}

impl RuntimeKind {
    /// The `TunnelManager` driving the live tunnel, regardless of source.
    pub fn manager(&self) -> &Arc<TunnelManager> {
        match self {
            RuntimeKind::Owned { manager, .. } | RuntimeKind::Borrowed { manager, .. } => manager,
        }
    }

    /// `true` when this tunnel was started from the Tunnels page (owns its
    /// connection). Used to decide whether a stop from the Tunnels page
    /// should fully tear down the session vs. just abort the borrowed manager.
    pub fn is_owned(&self) -> bool {
        matches!(self, RuntimeKind::Owned { .. })
    }

    /// The tab that lent its session, if this is a borrowed tunnel.
    pub fn borrowed_tab_id(&self) -> Option<u64> {
        match self {
            RuntimeKind::Borrowed { tab_id, .. } => Some(*tab_id),
            _ => None,
        }
    }
}

/// A central registry of all tunnels (stopped + running), held by `CrabportApp`.
///
/// The UI reads `list()` on every render to reflect live status. State
/// mutations go through the methods on `CrabportApp` (which owns the
/// registry), and those methods call `cx.notify()` after each mutation so
/// the Tunnels view re-renders. The registry itself is context-free — it's
/// just a `PlMutex<Vec<TunnelState>>`.
pub struct TunnelRegistry {
    tunnels: PlMutex<Vec<TunnelState>>,
}

impl TunnelRegistry {
    pub fn new() -> Self {
        Self {
            tunnels: PlMutex::new(Vec::new()),
        }
    }

    /// Replace the whole config list (e.g. after loading from the Store).
    /// Preserves any existing runtime state by matching on `id`.
    pub fn set_configs(&self, configs: Vec<TunnelEntry>) {
        let mut tunnels = self.tunnels.lock();
        let mut next = Vec::with_capacity(configs.len());
        for cfg in configs {
            let runtime = tunnels
                .iter()
                .find(|t| t.config.id == cfg.id)
                .and_then(|t| t.runtime.as_ref())
                .map(|r| match r {
                    RuntimeKind::Owned { session, manager } => RuntimeKind::Owned {
                        session: session.clone(),
                        manager: manager.clone(),
                    },
                    RuntimeKind::Borrowed { tab_id, manager } => RuntimeKind::Borrowed {
                        tab_id: *tab_id,
                        manager: manager.clone(),
                    },
                });
            next.push(TunnelState {
                config: cfg,
                runtime,
            });
        }
        *tunnels = next;
    }

    /// Add a freshly-persisted config.
    pub fn add(&self, config: TunnelEntry) {
        self.tunnels.lock().push(TunnelState {
            config,
            runtime: None,
        });
    }

    /// Update a config in place (preserving runtime state).
    pub fn update_config(&self, config: TunnelEntry) {
        let mut tunnels = self.tunnels.lock();
        if let Some(t) = tunnels.iter_mut().find(|t| t.config.id == config.id) {
            t.config = config;
        }
    }

    /// Remove a tunnel config + stop its runtime if any.
    pub async fn remove(&self, id: i64) {
        let removed = self
            .tunnels
            .lock()
            .iter()
            .find(|t| t.config.id == id)
            .and_then(|t| {
                t.runtime
                    .as_ref()
                    .map(|r| (r.manager().clone(), r.is_owned()))
            });
        if let Some((manager, _is_owned)) = removed {
            let _ = manager.stop_all().await;
        }
        self.tunnels.lock().retain(|t| t.config.id != id);
    }

    /// Snapshot for UI rendering.
    pub fn list(&self) -> Vec<TunnelView> {
        self.tunnels
            .lock()
            .iter()
            .map(|t| TunnelView {
                id: t.config.id,
                name: t.config.name.clone(),
                host_id: t.config.host_id,
                kind: t.config.kind,
                bind_addr: t.config.bind_addr.clone(),
                bind_port: t.config.bind_port,
                target_host: t.config.target_host.clone(),
                target_port: t.config.target_port,
                created_at: t.config.created_at,
                running: t.runtime.is_some(),
                borrowed_tab_id: t.runtime.as_ref().and_then(|r| r.borrowed_tab_id()),
                favorite: t.config.favorite,
                group_id: t.config.group_id,
            })
            .collect()
    }

    /// Is the given tunnel config currently running anywhere?
    pub fn is_running(&self, id: i64) -> bool {
        self.tunnels
            .lock()
            .iter()
            .any(|t| t.config.id == id && t.runtime.is_some())
    }

    /// Attach an owned runtime (started from Tunnels page).
    pub fn set_owned(&self, id: i64, session: Arc<OwnedSession>, manager: Arc<TunnelManager>) {
        let mut tunnels = self.tunnels.lock();
        if let Some(t) = tunnels.iter_mut().find(|t| t.config.id == id) {
            t.runtime = Some(RuntimeKind::Owned { session, manager });
        }
    }

    /// Attach a borrowed runtime (started from a terminal panel).
    pub fn set_borrowed(&self, id: i64, tab_id: u64, manager: Arc<TunnelManager>) {
        let mut tunnels = self.tunnels.lock();
        if let Some(t) = tunnels.iter_mut().find(|t| t.config.id == id) {
            t.runtime = Some(RuntimeKind::Borrowed { tab_id, manager });
        }
    }

    /// Clear runtime state for a tunnel (after stop).
    pub fn clear_runtime(&self, id: i64) {
        let mut tunnels = self.tunnels.lock();
        if let Some(t) = tunnels.iter_mut().find(|t| t.config.id == id) {
            t.runtime = None;
        }
    }

    /// Tear down all borrowed tunnels attached to a tab that's closing.
    /// Owned tunnels are left alone — they survive tab closure.
    pub async fn teardown_for_tab(&self, tab_id: u64) {
        let to_stop: Vec<Arc<TunnelManager>> = {
            let tunnels = self.tunnels.lock();
            tunnels
                .iter()
                .filter_map(|t| match &t.runtime {
                    Some(RuntimeKind::Borrowed {
                        tab_id: tid,
                        manager,
                    }) if *tid == tab_id => Some(manager.clone()),
                    _ => None,
                })
                .collect()
        };
        for manager in to_stop {
            let _ = manager.stop_all().await;
        }
        // Clear the runtime flags for those borrowed tunnels.
        let mut tunnels = self.tunnels.lock();
        for t in tunnels.iter_mut() {
            if matches!(&t.runtime, Some(RuntimeKind::Borrowed { tab_id: tid, .. }) if *tid == tab_id)
            {
                t.runtime = None;
            }
        }
        drop(tunnels);
    }

    /// Look up the runtime manager for a tunnel config id.
    ///
    /// Single lock pass — the previous two-lock implementation had a brief
    /// gap between releasing the first guard and acquiring the second that
    /// could race; collapsing to one lock removes the window entirely.
    pub fn manager_for(&self, id: i64) -> Option<Arc<TunnelManager>> {
        self.tunnels
            .lock()
            .iter()
            .find(|t| t.config.id == id)
            .and_then(|t| t.runtime.as_ref())
            .map(|r| r.manager().clone())
    }
}

/// A snapshot of a tunnel for UI rendering. Owned by the view, not the
/// registry, so it can be passed around without holding the lock.
#[derive(Clone)]
pub struct TunnelView {
    pub id: i64,
    pub name: String,
    pub host_id: i64,
    pub kind: crabport_core::credential::TunnelKind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub created_at: i64,
    pub running: bool,
    /// `Some(tab_id)` when this tunnel is borrowed from a terminal tab.
    pub borrowed_tab_id: Option<u64>,
    /// Starred by the user to pin it above un-starred tunnels.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped.
    pub group_id: Option<i64>,
}

/// Trait alias so `CrabportApp` can hold `Arc<dyn TunnelSource>` for the
/// borrowed-from-tab path. Implemented by `SshBackend`.
pub trait TunnelSource: CrabPortTunnel {}
impl<T: CrabPortTunnel + 'static> TunnelSource for T {}
