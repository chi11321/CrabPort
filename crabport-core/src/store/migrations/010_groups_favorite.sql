-- Migration 10: group favorites.
--
-- Adds a `favorite` column to `groups` so an entire group can be starred
-- and pinned above non-starred groups within the same kind.
ALTER TABLE groups ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
