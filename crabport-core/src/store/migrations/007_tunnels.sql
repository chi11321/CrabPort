-- Migration 7: add `tunnels` table. Each row is a persisted SSH
-- port-forwarding tunnel config, bound to a host via `host_id` (FK with
-- ON DELETE CASCADE so removing a host cleans up its tunnels). `kind` is
-- the forwarding direction (local/remote/dynamic); the bind_*/target_*
-- fields hold the port-forward parameters (their meaning depends on `kind`
-- — see `TunnelEntry`).
CREATE TABLE IF NOT EXISTS tunnels (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    host_id      INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL DEFAULT 'local',
    bind_addr    TEXT    NOT NULL DEFAULT '127.0.0.1',
    bind_port    INTEGER NOT NULL,
    target_host  TEXT    NOT NULL DEFAULT '',
    target_port  INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tunnels_host ON tunnels (host_id);
