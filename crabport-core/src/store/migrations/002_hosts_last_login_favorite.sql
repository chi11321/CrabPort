-- Migration 2: add last_login and favorite to hosts.
ALTER TABLE hosts ADD COLUMN last_login INTEGER;
ALTER TABLE hosts ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
