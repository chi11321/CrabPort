//! Proxy CRUD.
//!
//! Proxy configs live in their own `proxies` table so they can be shared
//! across hosts and managed independently (list / edit / delete without
//! touching host rows). Hosts reference a proxy via the `proxy_id` FK.

use rusqlite::{OptionalExtension, params};

use crate::credential::{ProxyConfig, ProxyEntry, ProxyKind};
use crate::store::StoreError;

use super::Store;

impl Store {
    // -------------------------------------------------------------------
    // Proxies CRUD
    // -------------------------------------------------------------------

    /// List all saved proxies, most-recently-created last (id ascending).
    pub fn proxies(&self) -> Result<Vec<ProxyEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, kind, host, port, username, password, created_at FROM proxies ORDER BY id",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(2)?;
                let username: Option<String> = row.get(5)?;
                let password: Option<Vec<u8>> = row.get(6)?;
                Ok(ProxyEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: parse_proxy_kind(&kind_str),
                    host: row.get(3)?,
                    port: row.get(4)?,
                    // Normalize empty string → None so callers don't have to.
                    username: username.filter(|s| !s.is_empty()),
                    password,
                    created_at: row.get(7)?,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Look up a single proxy by id. Returns `None` if not found.
    pub fn find_proxy(&self, id: i64) -> Result<Option<ProxyEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, kind, host, port, username, password, created_at FROM proxies WHERE id=?1",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        stmt.query_row(params![id], |row| {
            let kind_str: String = row.get(2)?;
            let username: Option<String> = row.get(5)?;
            let password: Option<Vec<u8>> = row.get(6)?;
            Ok(ProxyEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: parse_proxy_kind(&kind_str),
                host: row.get(3)?,
                port: row.get(4)?,
                username: username.filter(|s| !s.is_empty()),
                password,
                created_at: row.get(7)?,
            })
        })
        .optional()
        .map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Look up a proxy and return it as a decrypted, ready-to-use
    /// `ProxyConfig` (password resolved). Returns `None` if the row is
    /// missing or the password blob can't be decrypted.
    pub fn find_proxy_config(&self, id: i64) -> Result<Option<ProxyConfig>, StoreError> {
        match self.find_proxy(id)? {
            Some(entry) => Ok(Some(entry.to_config(&self.enc_key)?)),
            None => Ok(None),
        }
    }

    /// Insert a new proxy. The `password` field is encrypted before storage.
    /// Returns the new row id.
    pub fn add_proxy(&self, proxy: &ProxyEntry) -> Result<i64, StoreError> {
        let password_enc = match &proxy.password {
            Some(p) if !p.is_empty() => {
                Some(self.encrypt_field(&String::from_utf8_lossy(p).into_owned())?)
            }
            _ => None,
        };
        // `username` is NOT NULL in the schema — coerce `None` to "" so we
        // don't trip the constraint. (rusqlite binds `Option::None` as SQL
        // NULL, not the column DEFAULT.)
        let username = proxy.username.clone().unwrap_or_default();
        self.db
            .execute(
                "INSERT INTO proxies (name, kind, host, port, username, password, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    proxy.name,
                    proxy_kind_str(proxy.kind),
                    proxy.host,
                    proxy.port,
                    username,
                    password_enc,
                    proxy.created_at,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    /// Update an existing proxy. Re-encrypts the password if present.
    pub fn update_proxy(&self, proxy: &ProxyEntry) -> Result<(), StoreError> {
        let password_enc = match &proxy.password {
            Some(p) if !p.is_empty() => {
                Some(self.encrypt_field(&String::from_utf8_lossy(p).into_owned())?)
            }
            _ => None,
        };
        // See `add_proxy`: coerce `None` → "" for the NOT NULL column.
        let username = proxy.username.clone().unwrap_or_default();
        self.db
            .execute(
                "UPDATE proxies SET name=?1, kind=?2, host=?3, port=?4, username=?5, password=?6 WHERE id=?7",
                params![
                    proxy.name,
                    proxy_kind_str(proxy.kind),
                    proxy.host,
                    proxy.port,
                    username,
                    password_enc,
                    proxy.id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Delete a proxy. Hosts referencing it via `proxy_id` will have their
    /// `proxy_id` set to NULL (SQLite ON DELETE SET NULL).
    pub fn remove_proxy(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM proxies WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}

fn proxy_kind_str(k: ProxyKind) -> &'static str {
    match k {
        ProxyKind::None => "none",
        ProxyKind::Socks5 => "socks5",
        ProxyKind::Http => "http",
        ProxyKind::Https => "https",
    }
}

fn parse_proxy_kind(s: &str) -> ProxyKind {
    ProxyKind::from_str(s)
}
