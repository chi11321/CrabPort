//! AI provider secrets — per-provider encrypted API keys.
//!
//! One row per configured provider entry (`[[ai.providers]].id` from
//! `config.toml`), AES-256-GCM encrypted with the store's key file — the
//! same at-rest protection credentials get. Keys are keyed by provider, not
//! by model: one endpoint's key serves every model reachable through it.

use rusqlite::{OptionalExtension, params};

use crate::crypto;
use crate::store::StoreError;

use super::Store;

impl Store {
    /// Decrypted API key for `provider_id`; `None` when never stored.
    pub fn ai_api_key(&self, provider_id: &str) -> Result<Option<String>, StoreError> {
        let blob: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT api_key FROM ai_secrets WHERE provider_id = ?1",
                params![provider_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let Some(blob) = blob else {
            return Ok(None);
        };
        if blob.is_empty() {
            return Ok(Some(String::new()));
        }
        let plain = crypto::decrypt(&blob, &self.enc_key)?;
        let text = String::from_utf8(plain).map_err(|e| StoreError::Crypto(e.to_string()))?;
        Ok(Some(text))
    }

    /// Encrypt + upsert the API key for `provider_id`. An empty `key` stores
    /// an empty blob ("set but blank"), mirroring [`Store::encrypt_field`].
    pub fn set_ai_api_key(&self, provider_id: &str, key: &str) -> Result<(), StoreError> {
        let blob = self.encrypt_field(key)?;
        self.db
            .execute(
                "INSERT INTO ai_secrets (provider_id, api_key) VALUES (?1, ?2)
                 ON CONFLICT(provider_id) DO UPDATE SET api_key = excluded.api_key",
                params![provider_id, blob],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }

    /// Remove a provider's API key entirely — called when the provider entry
    /// is deleted so the key doesn't linger (even encrypted) on disk.
    #[allow(dead_code)]
    pub fn delete_ai_api_key(&self, provider_id: &str) -> Result<(), StoreError> {
        self.db
            .execute(
                "DELETE FROM ai_secrets WHERE provider_id = ?1",
                params![provider_id],
            )
            .map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(())
    }
}
