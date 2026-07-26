-- Migration 9: favorites + grouping.
--
-- Adds a shared `groups` table (discriminated by a `kind` column so the
-- same group name can be reused across hosts / snippets / tunnels without
-- collision), plus `favorite` + `group_id` columns on `snippets` and
-- `tunnels`. `hosts` already has `favorite` from migration 2; it only gets
-- `group_id` here.
--
-- `group_id` uses `ON DELETE SET NULL` so deleting a group drops its
-- members back to "ungrouped" rather than cascading deletion
-- (hosts/snippets/tunnels are user data — a group is just a label).
CREATE TABLE IF NOT EXISTS groups (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    kind       TEXT    NOT NULL DEFAULT 'host',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_groups_kind
    ON groups (kind, sort_order);

ALTER TABLE hosts ADD COLUMN group_id INTEGER REFERENCES groups(id) ON DELETE SET NULL;

ALTER TABLE snippets ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snippets ADD COLUMN group_id INTEGER REFERENCES groups(id) ON DELETE SET NULL;

ALTER TABLE tunnels ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tunnels ADD COLUMN group_id INTEGER REFERENCES groups(id) ON DELETE SET NULL;
