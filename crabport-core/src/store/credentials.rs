//! Credential CRUD + secret resolution.
//!
//! Credentials carry the encrypted secret material (password / private
//! key / passphrase). Secrets are encrypted at rest with AES-256-GCM and
//! only decrypted transiently when a connection is being established.

use rusqlite::{OptionalExtension, params};

use crate::credential::{CredentialEntry, CredentialKind, HostEntry};
use crate::crypto;
use crate::store::StoreError;

use super::Store;

impl Store {
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
