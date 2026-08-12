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
//! - [`tunnels`] — tunnel CRUD
//! - [`groups`] — shared group CRUD for hosts/snippets/tunnels
//! - [`history`] — connection-event history (size-capped)
//!
//! Schema migrations live in [`migrations`]: one `.sql` file per migration
//! under `store/migrations/`, registered in an ordered list. See that
//! module's docs for the append-only rule and the per-migration error
//! policy.

mod commands;
mod credentials;
mod groups;
mod history;
mod hosts;
mod migrations;
mod proxies;
mod ssh_import;
mod snippets;
mod tunnels;

pub use history::{ConnectionEvent, ConnectionStatus};
#[cfg(test)]
mod tests;

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

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
// Impl: open + encryption helpers
// ---------------------------------------------------------------------------

impl Store {
    /// Open (or create) the store at the platform data directory.
    pub fn open() -> Result<Self, StoreError> {
        let dir = default_data_dir()?;
        Self::open_at(dir)
    }

    /// Open (or create) the store at a custom directory.
    pub fn open_at(dir: PathBuf) -> Result<Self, StoreError> {
        tracing::info!("store: opening at {}", dir.display());
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
        migrations::run(&store.db)?;
        tracing::info!("store: opened {}", db_path.display());
        Ok(store)
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
pub fn default_data_dir() -> Result<PathBuf, StoreError> {
    let base =
        dirs::data_dir().ok_or_else(|| StoreError::Io("cannot determine data dir".into()))?;
    Ok(base.join("crabport"))
}
