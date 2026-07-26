-- Migration 5: add `updated_at` column to `command_history` for existing
-- databases. New databases already get it from migration 3, in which case
-- this ALTER fails and is ignored (best-effort migration). `updated_at` is
-- bumped when a duplicate command is re-run so LRU eviction keeps
-- frequently-used commands alive.
ALTER TABLE command_history ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
