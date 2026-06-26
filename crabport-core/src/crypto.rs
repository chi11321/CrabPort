//! AES-256-GCM encryption for credential secrets.
//!
//! The encryption key is a 32-byte random file (`.key`). Because the key file
//! itself is high-entropy, we use it directly as the AES key — no PBKDF2
//! derivation needed. This avoids the ~600k SHA256 iterations that would
//! otherwise block the main thread on every decrypt call.
//!
//! Format of encrypted blob:
//! ```text
//! [nonce: 12B] [ciphertext + tag: variable]
//! ```

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};

const NONCE_LEN: usize = 12;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encrypt plaintext using the 32-byte key directly.
pub fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError("AES key init failed".into()))?;

    let nonce_bytes = random_bytes(NONCE_LEN);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CryptoError(format!("encrypt failed: {e}")))?;

    // nonce || ciphertext
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a blob produced by [`encrypt`].
pub fn decrypt(blob: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() < NONCE_LEN {
        return Err(CryptoError("blob too short".into()));
    }

    let nonce_bytes = &blob[..NONCE_LEN];
    let ciphertext = &blob[NONCE_LEN..];

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError("AES key init failed".into()))?;

    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CryptoError(format!("decrypt failed: {e}")))
}

/// Generate a random key file content (32 bytes).
pub fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).expect("rng failure");
    key
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

fn random_bytes(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("rng failure");
    buf
}

#[derive(Debug)]
pub struct CryptoError(pub String);

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CryptoError: {}", self.0)
    }
}

impl std::error::Error for CryptoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = generate_key();
        let plaintext = b"hello secret world";
        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let encrypted = encrypt(b"secret", &key1).unwrap();
        assert!(decrypt(&encrypted, &key2).is_err());
    }
}
