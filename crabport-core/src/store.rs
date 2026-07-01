//! Persistent storage backed by SQLite.
//!
//! # File layout
//!
//! ```text
//! {data_dir}/crabport/
//!   crabport.db       — SQLite database (hosts + credentials + proxies + ...)
//!   .key              — AES-256 encryption key for credential secrets
//! ```
//!
//! The store is split across several files, each covering one domain:
//!
//! - [`hosts`] — host CRUD + login/favorite helpers
//! - [`proxies`] — proxy CRUD
//! - [`credentials`] — credential CRUD + secret resolution
//! - [`commands`] — per-host command history (LRU-capped)
//! - [`snippets`] — global command-snippet library

mod commands;
mod credentials;
mod hosts;
mod proxies;
mod snippets;

use std::fs;
use std::path::PathBuf;

use rusqlite::{Connection, params};

use crate::crypto;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct Store {
    pub(crate) db: Connection,
    #[allow(dead_code)]
    pub(crate) key_path: PathBuf,
    pub(crate) enc_key: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum StoreError {
    Io(String),
    Db(String),
    Crypto(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "IO: {e}"),
            StoreError::Db(e) => write!(f, "DB: {e}"),
            StoreError::Crypto(e) => write!(f, "Crypto: {e}"),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}

impl From<crypto::CryptoError> for StoreError {
    fn from(e: crypto::CryptoError) -> Self {
        StoreError::Crypto(e.0)
    }
}

// ---------------------------------------------------------------------------
// Impl: open + migrate + encryption helpers
// ---------------------------------------------------------------------------

impl Store {
    /// Open (or create) the store at the platform data directory.
    pub fn open() -> Result<Self, StoreError> {
        let dir = default_data_dir()?;
        Self::open_at(dir)
    }

    /// Open (or create) the store at a custom directory.
    pub fn open_at(dir: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(&dir).map_err(|e| StoreError::Io(e.to_string()))?;

        let db_path = dir.join("crabport.db");
        let key_path = dir.join(".key");

        let db = Connection::open(&db_path).map_err(|e| StoreError::Db(e.to_string()))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let enc_key = Self::read_or_create_key(&key_path)?;

        let store = Self {
            db,
            key_path,
            enc_key,
        };
        store.migrate()?;
        Ok(store)
    }

    // -------------------------------------------------------------------
    // Migrations
    // -------------------------------------------------------------------

    fn migrate(&self) -> Result<(), StoreError> {
        // Ensure the schema_version tracking table exists
        self.db
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let current: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Migration 1: initial schema
        if current < 1 {
            self.db
                .execute_batch(
                    "
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
                    ",
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        // Migration 2: add last_login and favorite to hosts
        if current < 2 {
            self.db
                .execute_batch(
                    "
                    ALTER TABLE hosts ADD COLUMN last_login INTEGER;
                    ALTER TABLE hosts ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;
                    ",
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        // Migration 3: command history table. One row per captured
        // command, scoped to a host (via host_id) so each connection keeps
        // its own history across app restarts. `created_at` is unix
        // seconds for ordering (most-recent-first on query).
        // `updated_at` is bumped when a duplicate command is re-run so the
        // LRU eviction (by `updated_at`) keeps frequently-used commands.
        if current < 3 {
            self.db
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS command_history (
                        id         INTEGER PRIMARY KEY AUTOINCREMENT,
                        host_id    INTEGER NOT NULL,
                        command    TEXT    NOT NULL,
                        created_at INTEGER NOT NULL,
                        updated_at INTEGER NOT NULL DEFAULT 0
                    );
                    CREATE INDEX IF NOT EXISTS idx_command_history_host
                        ON command_history (host_id, id DESC);
                    ",
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        // Migration 4: snippets table. Global code-snippet library — not
        // scoped to a host. `name` is the user-facing label, `command` is
        // the literal text to insert into the terminal.
        if current < 4 {
            self.db
                .execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS snippets (
                        id         INTEGER PRIMARY KEY AUTOINCREMENT,
                        name       TEXT    NOT NULL,
                        command    TEXT    NOT NULL,
                        created_at INTEGER NOT NULL
                    );
                    ",
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        // Migration 5: add `updated_at` column to `command_history` for
        // existing databases. New databases already get it from migration 3.
        // `updated_at` is bumped when a duplicate command is re-run so LRU
        // eviction keeps frequently-used commands alive.
        if current < 5 {
            // ALTER TABLE ... ADD COLUMN is idempotent-safe via try/catch:
            // if the column already exists (e.g. a fresh DB that ran
            // migration 3 with the column included), this errors and we
            // ignore it.
            let _ = self.db.execute_batch(
                "ALTER TABLE command_history ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;",
            );
        }

        // Migration 6: add `proxies` table + `proxy_id` FK on `hosts`.
        // Proxy configs live in their own table so they can be shared
        // across hosts and managed independently (list / edit / delete
        // without touching host rows).
        if current < 6 {
            self.db
                .execute_batch(
                    "
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
                    ",
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        // Record the latest migration version
        let latest = 6;
        if current < latest {
            self.db
                .execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![latest],
                )
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Encryption key management
    // -------------------------------------------------------------------

    fn read_or_create_key(path: &PathBuf) -> Result<Vec<u8>, StoreError> {
        if path.exists() {
            let key = fs::read(path).map_err(|e| StoreError::Io(e.to_string()))?;
            Ok(key)
        } else {
            let key = crypto::generate_key();
            fs::write(path, key).map_err(|e| StoreError::Io(e.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o600);
                fs::set_permissions(path, perms).ok();
            }
            Ok(key.to_vec())
        }
    }

    pub(crate) fn encrypt_field(&self, plaintext: &str) -> Result<Vec<u8>, StoreError> {
        if plaintext.is_empty() {
            return Ok(Vec::new());
        }
        crypto::encrypt(plaintext.as_bytes(), &self.enc_key).map_err(Into::into)
    }

    #[allow(dead_code)]
    pub(crate) fn decrypt_field(&self, blob: &[u8]) -> Result<String, StoreError> {
        if blob.is_empty() {
            return Ok(String::new());
        }
        let plain = crypto::decrypt(blob, &self.enc_key)?;
        String::from_utf8(plain).map_err(|e| StoreError::Crypto(e.to_string()))
    }
}

/// Platform-specific data directory.
fn default_data_dir() -> Result<PathBuf, StoreError> {
    let base =
        dirs::data_dir().ok_or_else(|| StoreError::Io("cannot determine data dir".into()))?;
    Ok(base.join("crabport"))
}
