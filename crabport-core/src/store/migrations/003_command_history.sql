-- Migration 3: command history table. One row per captured command, scoped
-- to a host (via host_id) so each connection keeps its own history across
-- app restarts. `created_at` is unix seconds for ordering
-- (most-recent-first on query). `updated_at` is bumped when a duplicate
-- command is re-run so the LRU eviction (by `updated_at`) keeps
-- frequently-used commands.
CREATE TABLE IF NOT EXISTS command_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id    INTEGER NOT NULL,
    command    TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_command_history_host
    ON command_history (host_id, id DESC);
