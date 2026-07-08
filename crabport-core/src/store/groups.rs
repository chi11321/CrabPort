//! Group CRUD.
//!
//! Groups are user-created folders for organizing hosts / snippets / tunnels
//! in their respective management views. The `groups` table is shared across
//! all three collections — the `kind` column (`GroupKind`) discriminates which
//! collection a group belongs to, so a "Production" host group is distinct
//! from a "Production" snippet group.
//!
//! Rows in `hosts` / `snippets` / `tunnels` reference a group via their
//! nullable `group_id` FK. `None` / NULL means "ungrouped" (shown at the top
//! level of the list, above all groups).

use rusqlite::{OptionalExtension, params};

use crate::credential::{GroupEntry, GroupKind};
use crate::store::StoreError;

use super::Store;

impl Store {
    // -------------------------------------------------------------------
    // Groups CRUD
    // -------------------------------------------------------------------

    /// List all groups of the given kind, ordered by `sort_order` then `id`.
    pub fn groups(&self, kind: GroupKind) -> Result<Vec<GroupEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, kind, sort_order, created_at, favorite FROM groups \n                 WHERE kind = ?1 ORDER BY favorite DESC, sort_order ASC, id ASC",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(params![kind.as_str()], |row| {
                let kind_str: String = row.get(2)?;
                Ok(GroupEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: GroupKind::from_str(&kind_str),
                    sort_order: row.get(3)?,
                    created_at: row.get(4)?,
                    favorite: row.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Look up a single group by id. Returns `None` if not found.
    pub fn find_group(&self, id: i64) -> Result<Option<GroupEntry>, StoreError> {
        self.db
            .query_row(
                "SELECT id, name, kind, sort_order, created_at, favorite FROM groups WHERE id = ?1",
                params![id],
                |row| {
                    let kind_str: String = row.get(2)?;
                    Ok(GroupEntry {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        kind: GroupKind::from_str(&kind_str),
                        sort_order: row.get(3)?,
                        created_at: row.get(4)?,
                        favorite: row.get::<_, i64>(5)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Insert a new group. `sort_order` defaults to the next available value
    /// within the same `kind` when `None`. Returns the new row id.
    pub fn add_group(
        &self,
        name: &str,
        kind: GroupKind,
        sort_order: Option<i64>,
    ) -> Result<i64, StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Append to the end of this kind's ordering when unspecified.
        let sort_order = match sort_order {
            Some(s) => s,
            None => self
                .db
                .query_row(
                    "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM groups WHERE kind = ?1",
                    params![kind.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(0),
        };
        self.db
            .execute(
                "INSERT INTO groups (name, kind, sort_order, created_at, favorite) VALUES (?1, ?2, ?3, ?4, 0)",
                params![name, kind.as_str(), sort_order, now],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    /// Rename a group and/or change its sort order.
    pub fn update_group(
        &self,
        id: i64,
        name: &str,
        sort_order: Option<i64>,
    ) -> Result<(), StoreError> {
        match sort_order {
            Some(s) => {
                self.db
                    .execute(
                        "UPDATE groups SET name = ?1, sort_order = ?2 WHERE id = ?3",
                        params![name, s, id],
                    )
                    .map_err(|e| StoreError::Db(e.to_string()))?;
            }
            None => {
                self.db
                    .execute(
                        "UPDATE groups SET name = ?1 WHERE id = ?2",
                        params![name, id],
                    )
                    .map_err(|e| StoreError::Db(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Toggle the favorite flag for a group. Favorite groups sort above
    /// non-favorite groups within the same kind.
    pub fn toggle_group_favorite(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE groups SET favorite = CASE WHEN favorite = 1 THEN 0 ELSE 1 END WHERE id = ?1",
                params![id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Delete a group. Rows in hosts/snippets/tunnels referencing it are
    // NULLed out (ON DELETE SET NULL) so they fall back to "ungrouped"
    /// rather than disappearing.
    pub fn remove_group(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM groups WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}
