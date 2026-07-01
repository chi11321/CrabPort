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
    pub fn add_snippet(&self, name: &str, command: &str) -> Result<i64, StoreError> {
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
                "INSERT INTO snippets (name, command, created_at) VALUES (?1, ?2, ?3)",
                params![name, command, now],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    /// Load all snippets, most-recently-created first.
    pub fn snippets(&self) -> Result<Vec<SnippetEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, command, created_at FROM snippets ORDER BY id DESC")
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SnippetEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    command: row.get(2)?,
                    created_at: row.get(3)?,
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

    /// Update an existing snippet's name + command.
    pub fn update_snippet(&self, id: i64, name: &str, command: &str) -> Result<(), StoreError> {
        let name = if name.trim().is_empty() {
            command
        } else {
            name
        };
        self.db
            .execute(
                "UPDATE snippets SET name = ?1, command = ?2 WHERE id = ?3",
                params![name, command, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}
