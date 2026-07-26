-- Connection-event history: one row per connection attempt (success or
-- failure). Host metadata (name / kind / address / port / username) is
-- denormalized into the row instead of referencing `hosts` by FK, so
-- events survive the deletion of the saved host they came from and
-- ad-hoc (unsaved) connections can be recorded the same way.
--
-- `status` is 'success' | 'failed'; `error` carries the failure message
-- (NULL on success). `created_at` is unix seconds.
CREATE TABLE IF NOT EXISTS connection_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    address TEXT NOT NULL,
    port INTEGER NOT NULL,
    username TEXT NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    created_at INTEGER NOT NULL
);
