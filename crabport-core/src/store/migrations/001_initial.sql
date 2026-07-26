-- Migration 1: initial schema.
CREATE TABLE IF NOT EXISTS hosts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    host          TEXT    NOT NULL,
    port          INTEGER NOT NULL DEFAULT 22,
    username      TEXT    NOT NULL DEFAULT '',
    credential_id INTEGER,
    kind          TEXT    NOT NULL DEFAULT 'Ssh'
);

CREATE TABLE IF NOT EXISTS credentials (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    kind         TEXT    NOT NULL DEFAULT 'Password',
    anonymous    INTEGER NOT NULL DEFAULT 0,
    secret       BLOB    NOT NULL,
    private_key  BLOB    NOT NULL DEFAULT '',
    public_key   BLOB    NOT NULL DEFAULT '',
    certificate  BLOB    NOT NULL DEFAULT ''
);
