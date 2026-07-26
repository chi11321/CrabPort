-- Migration 8: add `private_key_kind` to `credentials`. This records
-- whether the `private_key` BLOB holds literal PEM key material ('Content',
-- the historical default) or a filesystem path to a key file ('Path'). The
-- UI uses it to restore the value into the correct field (textarea vs
-- read-only path input) when editing a host. Existing rows default to
-- 'Content'. Best-effort: on a fresh DB the column may already exist.
ALTER TABLE credentials ADD COLUMN private_key_kind TEXT NOT NULL DEFAULT 'Content';
