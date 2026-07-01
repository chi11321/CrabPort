//! Per-host command history (LRU-capped).
//!
//! One row per captured command, scoped to a host (via `host_id`) so each
//! connection keeps its own history across app restarts. `created_at` is
//! unix seconds for ordering (most-recent-first on query). `updated_at`
//! is bumped when a duplicate command is re-run so the LRU eviction (by
//! `updated_at`) keeps frequently-used commands.

use rusqlite::params;

use crate::store::StoreError;

use super::Store;

impl Store {
    /// Maximum number of commands retained per host. When the limit is
    /// exceeded the entries with the oldest `updated_at` (LRU) are evicted.
    const MAX_COMMAND_HISTORY: usize = 300;

    /// Append a command to the persistent history for `host_id`.
    ///
    /// - **Duplicate command exists**: bump that row's `updated_at` to now
    ///   so it stays alive under LRU eviction and floats to the top of the
    ///   "most-recently-used" ordering. No new row is inserted.
    /// - **New command**: insert with `created_at = updated_at = now`.
    /// - **Over limit**: delete the oldest-by-`updated_at` rows beyond
    ///   [`MAX_COMMAND_HISTORY`](Self::MAX_COMMAND_HISTORY).
    ///
    /// The in-memory dedup in `TerminalSession` only skips consecutive
    /// duplicates; this Store-level dedup handles the non-consecutive case
    /// (re-running an older command) by promoting it instead of re-inserting.
    pub fn add_command(&self, host_id: i64, command: &str) -> Result<(), StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // If this exact command already exists for this host, bump its
        // `updated_at` (LRU promotion) instead of inserting a duplicate.
        let updated = self
            .db
            .execute(
                "UPDATE command_history SET updated_at = ?1 WHERE host_id = ?2 AND command = ?3",
                params![now, host_id, command],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        if updated > 0 {
            return Ok(());
        }

        // New command — insert.
        self.db
            .execute(
                "INSERT INTO command_history (host_id, command, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![host_id, command, now, now],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        // LRU eviction: keep the `MAX_COMMAND_HISTORY` most-recently-updated
        // rows per host, delete the rest.
        self.db
            .execute(
                "DELETE FROM command_history WHERE host_id = ?1 AND id NOT IN (
                    SELECT id FROM command_history WHERE host_id = ?1
                    ORDER BY updated_at DESC LIMIT ?2
                 )",
                params![host_id, Self::MAX_COMMAND_HISTORY as i64],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Load the command history for `host_id`, most-recently-used first
    /// (ordered by `updated_at` DESC so re-run commands float to the top).
    pub fn commands_for_host(&self, host_id: i64) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT command FROM command_history WHERE host_id = ?1 ORDER BY updated_at DESC",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt
            .query_map(params![host_id], |row| row.get::<_, String>(0))
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }
}
