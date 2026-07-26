//! Connection-event history (size-capped).
//!
//! One row per connection *attempt* — success or failure — recorded when
//! the attempt resolves (first `Connected` status, or an error/close
//! before ever connecting). Host metadata is denormalized into the row
//! (no FK into `hosts`) so events outlive the saved host they came from.
//! `created_at` is unix seconds; the list is most-recent-first.

use rusqlite::params;

use crate::credential::HostKind;
use crate::store::StoreError;

use super::Store;
use super::hosts::{host_kind_str, parse_host_kind};

/// Outcome of a connection attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Success,
    Failed,
}

/// One recorded connection attempt.
#[derive(Clone, Debug)]
pub struct ConnectionEvent {
    pub id: i64,
    /// Session name as shown on the tab (may equal the address for
    /// ad-hoc connections).
    pub name: String,
    pub kind: HostKind,
    /// Host / IP for SSH & Telnet, device path for Serial.
    pub address: String,
    /// 0 for Serial (no port concept).
    pub port: u16,
    /// Empty for Serial.
    pub username: String,
    pub status: ConnectionStatus,
    /// Failure message; `None` on success.
    pub error: Option<String>,
    /// Unix seconds. Set by [`Store::add_connection_event`] — the value
    /// on the passed-in event is ignored on insert.
    pub created_at: i64,
}

fn status_str(s: ConnectionStatus) -> &'static str {
    match s {
        ConnectionStatus::Success => "success",
        ConnectionStatus::Failed => "failed",
    }
}

fn parse_status(s: &str) -> ConnectionStatus {
    match s {
        "failed" => ConnectionStatus::Failed,
        _ => ConnectionStatus::Success,
    }
}

impl Store {
    /// Maximum number of connection events retained. When exceeded the
    /// oldest rows (by id — insertion order) are evicted.
    const MAX_CONNECTION_HISTORY: usize = 100;

    /// Record a connection attempt. Returns the new row id.
    pub fn add_connection_event(&self, event: &ConnectionEvent) -> Result<i64, StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.db
            .execute(
                "INSERT INTO connection_history \
                 (name, kind, address, port, username, status, error, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.name,
                    host_kind_str(event.kind),
                    event.address,
                    event.port,
                    event.username,
                    status_str(event.status),
                    event.error,
                    now,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let id = self.db.last_insert_rowid();

        // Size cap: keep the newest MAX_CONNECTION_HISTORY rows.
        self.db
            .execute(
                "DELETE FROM connection_history WHERE id NOT IN (
                    SELECT id FROM connection_history ORDER BY id DESC LIMIT ?1
                 )",
                params![Self::MAX_CONNECTION_HISTORY as i64],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(id)
    }

    /// Load all connection events, most-recent-first.
    pub fn connection_history(&self) -> Result<Vec<ConnectionEvent>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, kind, address, port, username, status, error, created_at \
                 FROM connection_history ORDER BY id DESC",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(2)?;
                let status_str: String = row.get(6)?;
                Ok(ConnectionEvent {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: parse_host_kind(&kind_str),
                    address: row.get(3)?,
                    port: row.get(4)?,
                    username: row.get(5)?,
                    status: parse_status(&status_str),
                    error: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete a single connection event.
    pub fn remove_connection_event(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM connection_history WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Delete every connection event.
    pub fn clear_connection_history(&self) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM connection_history", [])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "crabport-history-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open_at(dir.clone()).expect("open temp store");
        (store, dir)
    }

    fn event(name: &str, kind: HostKind, status: ConnectionStatus) -> ConnectionEvent {
        ConnectionEvent {
            id: 0,
            name: name.to_string(),
            kind,
            address: "example.com".to_string(),
            port: 22,
            username: "root".to_string(),
            status,
            error: (status == ConnectionStatus::Failed).then(|| "auth failed".to_string()),
            created_at: 0,
        }
    }

    /// Round-trips all fields, orders most-recent-first, and delete /
    /// clear behave as expected.
    #[test]
    fn add_list_remove_clear_roundtrip() {
        let (s, dir) = open_temp("crud");

        let a = s
            .add_connection_event(&event("first", HostKind::Ssh, ConnectionStatus::Success))
            .unwrap();
        let b = s
            .add_connection_event(&event("second", HostKind::Telnet, ConnectionStatus::Failed))
            .unwrap();
        assert!(b > a);

        let list = s.connection_history().unwrap();
        assert_eq!(list.len(), 2);
        // Most-recent-first.
        assert_eq!(list[0].name, "second");
        assert_eq!(list[0].kind, HostKind::Telnet);
        assert_eq!(list[0].status, ConnectionStatus::Failed);
        assert_eq!(list[0].error.as_deref(), Some("auth failed"));
        assert_eq!(list[1].name, "first");
        assert_eq!(list[1].status, ConnectionStatus::Success);
        assert_eq!(list[1].error, None);
        assert_eq!(list[1].address, "example.com");
        assert_eq!(list[1].port, 22);
        assert_eq!(list[1].username, "root");
        assert!(list[1].created_at > 0);

        s.remove_connection_event(a).unwrap();
        assert_eq!(s.connection_history().unwrap().len(), 1);

        s.clear_connection_history().unwrap();
        assert!(s.connection_history().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The size cap evicts the oldest rows, keeping the newest
    /// `MAX_CONNECTION_HISTORY`.
    #[test]
    fn size_cap_evicts_oldest() {
        let (s, dir) = open_temp("cap");

        let extra = 7;
        for i in 0..Store::MAX_CONNECTION_HISTORY + extra {
            let mut e = event(
                &format!("conn-{i}"),
                HostKind::Ssh,
                ConnectionStatus::Success,
            );
            e.port = (i % u16::MAX as usize) as u16;
            s.add_connection_event(&e).unwrap();
        }

        let list = s.connection_history().unwrap();
        assert_eq!(list.len(), Store::MAX_CONNECTION_HISTORY);
        // Newest survives, oldest `extra` rows are gone.
        assert_eq!(
            list[0].name,
            format!("conn-{}", Store::MAX_CONNECTION_HISTORY + extra - 1)
        );
        assert_eq!(list.last().unwrap().name, format!("conn-{extra}"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
