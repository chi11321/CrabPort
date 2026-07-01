//! Host CRUD + login/favorite helpers.
//!
//! `impl Store` extension — these methods live here rather than in
//! `store.rs` to keep the root file focused on open/migrate/encrypt.

use rusqlite::{OptionalExtension, params};

use crate::credential::{HostEntry, HostKind};
use crate::store::StoreError;

use super::Store;

impl Store {
    // -------------------------------------------------------------------
    // Hosts CRUD
    // -------------------------------------------------------------------

    pub fn hosts(&self) -> Result<Vec<HostEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host, port, username, credential_id, kind, last_login, favorite, proxy_id FROM hosts ORDER BY favorite DESC, last_login DESC, id",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(6)?;
                let last_login: Option<i64> = row.get(7)?;
                let favorite: i64 = row.get(8)?;
                let proxy_id: Option<i64> = row.get(9)?;
                Ok(HostEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    host: row.get(2)?,
                    port: row.get(3)?,
                    username: row.get(4)?,
                    credential_id: row.get(5)?,
                    kind: parse_host_kind(&kind_str),
                    last_login,
                    favorite: favorite != 0,
                    proxy_id,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn add_host(&self, host: &HostEntry) -> Result<i64, StoreError> {
        self.db
            .execute(
                "INSERT INTO hosts (name, host, port, username, credential_id, kind, last_login, favorite, proxy_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    host.name,
                    host.host,
                    host.port,
                    host.username,
                    host.credential_id,
                    host_kind_str(host.kind),
                    host.last_login,
                    host.favorite as i64,
                    host.proxy_id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn remove_host(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM hosts WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn update_host(&self, host: &HostEntry) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE hosts SET name=?1, host=?2, port=?3, username=?4, credential_id=?5, kind=?6, last_login=?7, favorite=?8, proxy_id=?9 WHERE id=?10",
                params![
                    host.name,
                    host.host,
                    host.port,
                    host.username,
                    host.credential_id,
                    host_kind_str(host.kind),
                    host.last_login,
                    host.favorite as i64,
                    host.proxy_id,
                    host.id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn find_host(&self, id: i64) -> Result<Option<HostEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host, port, username, credential_id, kind, last_login, favorite, proxy_id FROM hosts WHERE id=?1",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        stmt.query_row(params![id], |row| {
            let kind_str: String = row.get(6)?;
            let last_login: Option<i64> = row.get(7)?;
            let favorite: i64 = row.get(8)?;
            let proxy_id: Option<i64> = row.get(9)?;
            Ok(HostEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                username: row.get(4)?,
                credential_id: row.get(5)?,
                kind: parse_host_kind(&kind_str),
                last_login,
                favorite: favorite != 0,
                proxy_id,
            })
        })
        .optional()
        .map_err(|e| StoreError::Db(e.to_string()))
    }

    // -------------------------------------------------------------------
    // Host login / favorite helpers
    // -------------------------------------------------------------------

    /// Update the last_login timestamp for a host to the current time (unix epoch seconds).
    pub fn touch_host_login(&self, id: i64) -> Result<(), StoreError> {
        let now = chrono::Utc::now().timestamp();
        self.db
            .execute(
                "UPDATE hosts SET last_login = ?1 WHERE id = ?2",
                params![now, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Toggle the favorite flag for a host.
    pub fn toggle_host_favorite(&self, id: i64) -> Result<bool, StoreError> {
        let host = self
            .find_host(id)?
            .ok_or_else(|| StoreError::Db("host not found".into()))?;
        let new_val = !host.favorite;
        self.db
            .execute(
                "UPDATE hosts SET favorite = ?1 WHERE id = ?2",
                params![new_val as i64, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(new_val)
    }
}

fn host_kind_str(k: HostKind) -> &'static str {
    match k {
        HostKind::Ssh => "Ssh",
        HostKind::Telnet => "Telnet",
        HostKind::Serial => "Serial",
    }
}

fn parse_host_kind(s: &str) -> HostKind {
    match s {
        "Telnet" => HostKind::Telnet,
        "Serial" => HostKind::Serial,
        _ => HostKind::Ssh,
    }
}
