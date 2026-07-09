//! Global command-snippet library.
//!
//! Snippets are not scoped to a host — they form a reusable library of
//! commands accessible from any connection. `name` is the user-facing
//! label, `command` is the literal text to insert into the terminal.

use rusqlite::params;

use crate::credential::SnippetEntry;
use crate::store::StoreError;

use super::Store;

impl Store {
    /// Insert a new snippet. `name` defaults to the command text when empty.
    pub fn add_snippet(
        &self,
        name: &str,
        command: &str,
        favorite: bool,
        group_id: Option<i64>,
    ) -> Result<i64, StoreError> {
        let name = if name.trim().is_empty() {
            command
        } else {
            name
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.db
            .execute(
                "INSERT INTO snippets (name, command, created_at, favorite, group_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, command, now, favorite as i64, group_id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    /// Load all snippets, favorites first then most-recently-created.
    pub fn snippets(&self) -> Result<Vec<SnippetEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, command, created_at, favorite, group_id FROM snippets ORDER BY favorite DESC, id DESC")
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let favorite: i64 = row.get(4)?;
                Ok(SnippetEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    command: row.get(2)?,
                    created_at: row.get(3)?,
                    favorite: favorite != 0,
                    group_id: row.get(5)?,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete a snippet by id.
    pub fn remove_snippet(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM snippets WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Update an existing snippet's name, command, favorite, and group.
    pub fn update_snippet(
        &self,
        id: i64,
        name: &str,
        command: &str,
        favorite: bool,
        group_id: Option<i64>,
    ) -> Result<(), StoreError> {
        let name = if name.trim().is_empty() {
            command
        } else {
            name
        };
        self.db
            .execute(
                "UPDATE snippets SET name = ?1, command = ?2, favorite = ?3, group_id = ?4 WHERE id = ?5",
                params![name, command, favorite as i64, group_id, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Toggle the favorite flag for a snippet. Returns the new value.
    pub fn toggle_snippet_favorite(&self, id: i64) -> Result<bool, StoreError> {
        let current: i64 = self
            .db
            .query_row(
                "SELECT favorite FROM snippets WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let new_val = current == 0;
        self.db
            .execute(
                "UPDATE snippets SET favorite = ?1 WHERE id = ?2",
                params![new_val as i64, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(new_val)
    }

    /// Move a snippet to a different group (`None` = ungrouped).
    pub fn set_snippet_group(&self, id: i64, group_id: Option<i64>) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE snippets SET group_id = ?1 WHERE id = ?2",
                params![group_id, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}
