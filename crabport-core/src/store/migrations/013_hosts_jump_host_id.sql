-- Migration 13: add `jump_host_id` to `hosts`.
--
-- FK back into `hosts` — the jump host (bastion) this host connects
-- through, mirroring OpenSSH's `ProxyJump`. `ON DELETE SET NULL` so
-- deleting a jump host degrades its dependents to direct connections
-- instead of cascading. NULL (the default) means direct connection.
-- Best-effort: on a fresh DB the column may already exist.
ALTER TABLE hosts ADD COLUMN jump_host_id INTEGER REFERENCES hosts(id) ON DELETE SET NULL;
