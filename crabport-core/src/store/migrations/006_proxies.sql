-- Migration 6: add `proxies` table + `proxy_id` FK on `hosts`. Proxy
-- configs live in their own table so they can be shared across hosts and
-- managed independently (list / edit / delete without touching host rows).
CREATE TABLE IF NOT EXISTS proxies (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT    NOT NULL,
    kind      TEXT    NOT NULL DEFAULT 'none',
    host      TEXT    NOT NULL DEFAULT '',
    port      INTEGER NOT NULL DEFAULT 0,
    username  TEXT    NOT NULL DEFAULT '',
    password  BLOB,
    created_at INTEGER NOT NULL DEFAULT 0
);
ALTER TABLE hosts ADD COLUMN proxy_id INTEGER REFERENCES proxies(id);
