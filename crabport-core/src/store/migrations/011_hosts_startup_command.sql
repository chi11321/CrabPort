-- Migration 11: add `startup_command` to `hosts`.
--
-- Stores an optional command (or multi-line script) the user wants to run
-- automatically once the shell/terminal session is ready — after the SSH
-- shell starts or the Telnet TCP connection is established. Empty string
-- (the column default) means no startup command. Best-effort: on a fresh
-- DB the column may already exist.
ALTER TABLE hosts ADD COLUMN startup_command TEXT NOT NULL DEFAULT '';
