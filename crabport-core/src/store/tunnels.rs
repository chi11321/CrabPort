//! Tunnel CRUD.
//!
//! Tunnel configs live in their own `tunnels` table, each bound to a host via
//! the `host_id` FK (ON DELETE CASCADE). A tunnel describes an SSH
//! port-forwarding configuration (`ssh -L` / `ssh -R` / `ssh -D`); the actual
//! forwarding session is established at start time, either from a fresh
//! independent SSH connection or by borrowing an already-connected tab's
//! session.

use rusqlite::{OptionalExtension, params};

use crate::credential::{TunnelEntry, TunnelKind};
use crate::store::StoreError;

use super::Store;

impl Store {
    // -------------------------------------------------------------------
    // Tunnels CRUD
    // -------------------------------------------------------------------

    /// List all saved tunnels, ordered by favorite then id ascending.
    pub fn tunnels(&self) -> Result<Vec<TunnelEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host_id, kind, bind_addr, bind_port, target_host, target_port, created_at, favorite, group_id FROM tunnels ORDER BY favorite DESC, id",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(3)?;
                let favorite: i64 = row.get(9)?;
                Ok(TunnelEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    host_id: row.get(2)?,
                    kind: parse_tunnel_kind(&kind_str),
                    bind_addr: row.get(4)?,
                    bind_port: row.get(5)?,
                    target_host: row.get(6)?,
                    target_port: row.get(7)?,
                    created_at: row.get(8)?,
                    favorite: favorite != 0,
                    group_id: row.get(10)?,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// List all tunnels belonging to a given host, ordered by favorite then id ascending.
    pub fn tunnels_for_host(&self, host_id: i64) -> Result<Vec<TunnelEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host_id, kind, bind_addr, bind_port, target_host, target_port, created_at, favorite, group_id FROM tunnels WHERE host_id=?1 ORDER BY favorite DESC, id",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let rows = stmt
            .query_map(params![host_id], |row| {
                let kind_str: String = row.get(3)?;
                let favorite: i64 = row.get(9)?;
                Ok(TunnelEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    host_id: row.get(2)?,
                    kind: parse_tunnel_kind(&kind_str),
                    bind_addr: row.get(4)?,
                    bind_port: row.get(5)?,
                    target_host: row.get(6)?,
                    target_port: row.get(7)?,
                    created_at: row.get(8)?,
                    favorite: favorite != 0,
                    group_id: row.get(10)?,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Look up a single tunnel by id. Returns `None` if not found.
    pub fn find_tunnel(&self, id: i64) -> Result<Option<TunnelEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host_id, kind, bind_addr, bind_port, target_host, target_port, created_at, favorite, group_id FROM tunnels WHERE id=?1",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        stmt.query_row(params![id], |row| {
            let kind_str: String = row.get(3)?;
            let favorite: i64 = row.get(9)?;
            Ok(TunnelEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                host_id: row.get(2)?,
                kind: parse_tunnel_kind(&kind_str),
                bind_addr: row.get(4)?,
                bind_port: row.get(5)?,
                target_host: row.get(6)?,
                target_port: row.get(7)?,
                created_at: row.get(8)?,
                favorite: favorite != 0,
                group_id: row.get(10)?,
            })
        })
        .optional()
        .map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Insert a new tunnel. Returns the new row id.
    pub fn add_tunnel(&self, tunnel: &TunnelEntry) -> Result<i64, StoreError> {
        // `bind_addr`/`target_host` are NOT NULL in the schema — coerce any
        // unset value to the column default rather than tripping the
        // constraint. (rusqlite binds `None` as SQL NULL, not the DEFAULT.)
        let bind_addr = if tunnel.bind_addr.is_empty() {
            "127.0.0.1".to_string()
        } else {
            tunnel.bind_addr.clone()
        };
        self.db
            .execute(
                "INSERT INTO tunnels (name, host_id, kind, bind_addr, bind_port, target_host, target_port, created_at, favorite, group_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    tunnel.name,
                    tunnel.host_id,
                    tunnel_kind_str(tunnel.kind),
                    bind_addr,
                    tunnel.bind_port,
                    tunnel.target_host,
                    tunnel.target_port,
                    tunnel.created_at,
                    tunnel.favorite as i64,
                    tunnel.group_id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    /// Update an existing tunnel.
    pub fn update_tunnel(&self, tunnel: &TunnelEntry) -> Result<(), StoreError> {
        // See `add_tunnel`: coerce empty `bind_addr` to the default.
        let bind_addr = if tunnel.bind_addr.is_empty() {
            "127.0.0.1".to_string()
        } else {
            tunnel.bind_addr.clone()
        };
        self.db
            .execute(
                "UPDATE tunnels SET name=?1, host_id=?2, kind=?3, bind_addr=?4, bind_port=?5, target_host=?6, target_port=?7, favorite=?8, group_id=?9 WHERE id=?10",
                params![
                    tunnel.name,
                    tunnel.host_id,
                    tunnel_kind_str(tunnel.kind),
                    bind_addr,
                    tunnel.bind_port,
                    tunnel.target_host,
                    tunnel.target_port,
                    tunnel.favorite as i64,
                    tunnel.group_id,
                    tunnel.id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Toggle the favorite flag for a tunnel. Returns the new value.
    pub fn toggle_tunnel_favorite(&self, id: i64) -> Result<bool, StoreError> {
        let current: i64 = self
            .db
            .query_row(
                "SELECT favorite FROM tunnels WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let new_val = current == 0;
        self.db
            .execute(
                "UPDATE tunnels SET favorite = ?1 WHERE id = ?2",
                params![new_val as i64, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(new_val)
    }

    /// Move a tunnel to a different group (`None` = ungrouped).
    pub fn set_tunnel_group(&self, id: i64, group_id: Option<i64>) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE tunnels SET group_id = ?1 WHERE id = ?2",
                params![group_id, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Delete a tunnel. Tunnels are scoped to a host via `host_id` (FK with
    /// ON DELETE CASCADE), so deleting a host also removes its tunnels.
    pub fn remove_tunnel(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM tunnels WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}

fn tunnel_kind_str(k: TunnelKind) -> &'static str {
    TunnelKind::as_str(&k)
}

fn parse_tunnel_kind(s: &str) -> TunnelKind {
    TunnelKind::from_str(s)
}
