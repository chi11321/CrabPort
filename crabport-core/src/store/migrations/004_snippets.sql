-- Migration 4: snippets table. Global code-snippet library — not scoped to
-- a host. `name` is the user-facing label, `command` is the literal text to
-- insert into the terminal.
CREATE TABLE IF NOT EXISTS snippets (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    command    TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);
