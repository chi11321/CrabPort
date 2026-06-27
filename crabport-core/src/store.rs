//! Persistent storage backed by SQLite.
//!
//! # File layout
//!
//! ```text
//! {data_dir}/crabport/
//!   crabport.db       — SQLite database (hosts + credentials)
//!   .key              — AES-256 encryption key for credential secrets
//! ```

use std::fs;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};

use crate::credential::{CredentialEntry, CredentialKind, HostEntry, HostKind};
use crate::crypto;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct Store {
    db: Connection,
    #[allow(dead_code)]
    key_path: PathBuf,
    enc_key: Vec<u8>,
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
// Impl
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

        // Record the latest migration version
        let latest = 2;
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

    fn encrypt_field(&self, plaintext: &str) -> Result<Vec<u8>, StoreError> {
        if plaintext.is_empty() {
            return Ok(Vec::new());
        }
        crypto::encrypt(plaintext.as_bytes(), &self.enc_key).map_err(Into::into)
    }

    #[allow(dead_code)]
    fn decrypt_field(&self, blob: &[u8]) -> Result<String, StoreError> {
        if blob.is_empty() {
            return Ok(String::new());
        }
        let plain = crypto::decrypt(blob, &self.enc_key)?;
        String::from_utf8(plain).map_err(|e| StoreError::Crypto(e.to_string()))
    }

    // -------------------------------------------------------------------
    // Hosts CRUD
    // -------------------------------------------------------------------

    pub fn hosts(&self) -> Result<Vec<HostEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host, port, username, credential_id, kind, last_login, favorite FROM hosts ORDER BY favorite DESC, last_login DESC, id",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(6)?;
                let last_login: Option<i64> = row.get(7)?;
                let favorite: i64 = row.get(8)?;
                Ok(HostEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    host: row.get(2)?,
                    port: row.get(3)?,
                    username: row.get(4)?,
                    credential_id: row.get(5)?,
                    kind: parse_host_kind(&kind_str),
                    last_login,
                    favorite: favorite != 0,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn add_host(&self, host: &HostEntry) -> Result<i64, StoreError> {
        self.db
            .execute(
                "INSERT INTO hosts (name, host, port, username, credential_id, kind, last_login, favorite) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    host.name,
                    host.host,
                    host.port,
                    host.username,
                    host.credential_id,
                    host_kind_str(host.kind),
                    host.last_login,
                    host.favorite as i64,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn remove_host(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM hosts WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn update_host(&self, host: &HostEntry) -> Result<(), StoreError> {
        self.db
            .execute(
                "UPDATE hosts SET name=?1, host=?2, port=?3, username=?4, credential_id=?5, kind=?6, last_login=?7, favorite=?8 WHERE id=?9",
                params![
                    host.name,
                    host.host,
                    host.port,
                    host.username,
                    host.credential_id,
                    host_kind_str(host.kind),
                    host.last_login,
                    host.favorite as i64,
                    host.id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn find_host(&self, id: i64) -> Result<Option<HostEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, name, host, port, username, credential_id, kind, last_login, favorite FROM hosts WHERE id=?1",
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;

        stmt.query_row(params![id], |row| {
            let kind_str: String = row.get(6)?;
            let last_login: Option<i64> = row.get(7)?;
            let favorite: i64 = row.get(8)?;
            Ok(HostEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                username: row.get(4)?,
                credential_id: row.get(5)?,
                kind: parse_host_kind(&kind_str),
                last_login,
                favorite: favorite != 0,
            })
        })
        .optional()
        .map_err(|e| StoreError::Db(e.to_string()))
    }

    // -------------------------------------------------------------------
    // Credentials CRUD
    // -------------------------------------------------------------------

    /// Update the last_login timestamp for a host to the current time (unix epoch seconds).
    pub fn touch_host_login(&self, id: i64) -> Result<(), StoreError> {
        let now = chrono::Utc::now().timestamp();
        self.db
            .execute(
                "UPDATE hosts SET last_login = ?1 WHERE id = ?2",
                params![now, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Toggle the favorite flag for a host.
    pub fn toggle_host_favorite(&self, id: i64) -> Result<bool, StoreError> {
        let host = self
            .find_host(id)?
            .ok_or_else(|| StoreError::Db("host not found".into()))?;
        let new_val = !host.favorite;
        self.db
            .execute(
                "UPDATE hosts SET favorite = ?1 WHERE id = ?2",
                params![new_val as i64, id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(new_val)
    }

    // -------------------------------------------------------------------
    // Credentials CRUD
    // -------------------------------------------------------------------

    pub fn credentials(&self) -> Result<Vec<CredentialEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, kind, anonymous, secret, private_key, public_key, certificate FROM credentials ORDER BY id")
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let key = self.enc_key.clone();
        let rows = stmt
            .query_map([], |row| {
                let kind_str: String = row.get(2)?;
                let anon: bool = row.get::<_, i64>(3)? != 0;
                let secret_blob: Vec<u8> = row.get(4)?;
                let pk_blob: Vec<u8> = row.get(5)?;
                let pubk_blob: Vec<u8> = row.get(6)?;
                let cert_blob: Vec<u8> = row.get(7)?;

                // Decrypt outside query_map to avoid capturing self
                let secret = if secret_blob.is_empty() {
                    String::new()
                } else {
                    crypto::decrypt(&secret_blob, &key)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };
                let private_key = if pk_blob.is_empty() {
                    String::new()
                } else {
                    crypto::decrypt(&pk_blob, &key)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };
                let public_key = if pubk_blob.is_empty() {
                    String::new()
                } else {
                    crypto::decrypt(&pubk_blob, &key)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };
                let certificate = if cert_blob.is_empty() {
                    String::new()
                } else {
                    crypto::decrypt(&cert_blob, &key)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                };

                Ok(CredentialEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: parse_cred_kind(&kind_str),
                    anonymous: anon,
                    secret,
                    private_key,
                    public_key,
                    certificate,
                })
            })
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| StoreError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn add_credential(&self, cred: &CredentialEntry) -> Result<i64, StoreError> {
        let secret_enc = self.encrypt_field(&cred.secret)?;
        let pk_enc = self.encrypt_field(&cred.private_key)?;
        let pubk_enc = self.encrypt_field(&cred.public_key)?;
        let cert_enc = self.encrypt_field(&cred.certificate)?;

        self.db
            .execute(
                "INSERT INTO credentials (name, kind, anonymous, secret, private_key, public_key, certificate) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    cred.name,
                    cred_kind_str(cred.kind),
                    cred.anonymous as i64,
                    secret_enc,
                    pk_enc,
                    pubk_enc,
                    cert_enc,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn remove_credential(&self, id: i64) -> Result<(), StoreError> {
        self.db
            .execute("DELETE FROM credentials WHERE id = ?1", params![id])
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn update_credential(&self, cred: &CredentialEntry) -> Result<(), StoreError> {
        let secret_enc = self.encrypt_field(&cred.secret)?;
        let pk_enc = self.encrypt_field(&cred.private_key)?;
        let pubk_enc = self.encrypt_field(&cred.public_key)?;
        let cert_enc = self.encrypt_field(&cred.certificate)?;

        self.db
            .execute(
                "UPDATE credentials SET name=?1, kind=?2, anonymous=?3, secret=?4, private_key=?5, public_key=?6, certificate=?7 WHERE id=?8",
                params![
                    cred.name,
                    cred_kind_str(cred.kind),
                    cred.anonymous as i64,
                    secret_enc,
                    pk_enc,
                    pubk_enc,
                    cert_enc,
                    cred.id,
                ],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn find_credential(&self, id: i64) -> Result<Option<CredentialEntry>, StoreError> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, kind, anonymous, secret, private_key, public_key, certificate FROM credentials WHERE id=?1")
            .map_err(|e| StoreError::Db(e.to_string()))?;

        let key = self.enc_key.clone();
        stmt.query_row(params![id], |row| {
            let kind_str: String = row.get(2)?;
            let anon: bool = row.get::<_, i64>(3)? != 0;
            let secret_blob: Vec<u8> = row.get(4)?;
            let pk_blob: Vec<u8> = row.get(5)?;
            let pubk_blob: Vec<u8> = row.get(6)?;
            let cert_blob: Vec<u8> = row.get(7)?;

            let secret = if secret_blob.is_empty() {
                String::new()
            } else {
                crypto::decrypt(&secret_blob, &key)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            };
            let private_key = if pk_blob.is_empty() {
                String::new()
            } else {
                crypto::decrypt(&pk_blob, &key)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            };
            let public_key = if pubk_blob.is_empty() {
                String::new()
            } else {
                crypto::decrypt(&pubk_blob, &key)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            };
            let certificate = if cert_blob.is_empty() {
                String::new()
            } else {
                crypto::decrypt(&cert_blob, &key)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            };

            Ok(CredentialEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: parse_cred_kind(&kind_str),
                anonymous: anon,
                secret,
                private_key,
                public_key,
                certificate,
            })
        })
        .optional()
        .map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Resolve the decrypted secret for a host by looking up its linked credential.
    pub fn resolve_secret(&self, host: &HostEntry) -> Result<Option<String>, StoreError> {
        let cred_id = match host.credential_id {
            Some(id) => id,
            None => return Ok(None),
        };
        let cred = self.find_credential(cred_id)?;
        Ok(cred.map(|c| c.secret))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn host_kind_str(k: HostKind) -> &'static str {
    match k {
        HostKind::Ssh => "Ssh",
        HostKind::Telnet => "Telnet",
        HostKind::Serial => "Serial",
    }
}

fn parse_host_kind(s: &str) -> HostKind {
    match s {
        "Telnet" => HostKind::Telnet,
        "Serial" => HostKind::Serial,
        _ => HostKind::Ssh,
    }
}

fn cred_kind_str(k: CredentialKind) -> &'static str {
    match k {
        CredentialKind::Password => "Password",
        CredentialKind::Certificate => "Certificate",
    }
}

fn parse_cred_kind(s: &str) -> CredentialKind {
    match s {
        "Certificate" => CredentialKind::Certificate,
        _ => CredentialKind::Password,
    }
}

/// Platform-specific data directory.
fn default_data_dir() -> Result<PathBuf, StoreError> {
    let base =
        dirs::data_dir().ok_or_else(|| StoreError::Io("cannot determine data dir".into()))?;
    Ok(base.join("crabport"))
}
