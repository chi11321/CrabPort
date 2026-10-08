use russh::keys::key::KeyPair;
use std::fmt;
use std::path::{Path, PathBuf};

/// Result of decoding a private-key file without a password phrase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateKeyFileStatus {
    /// The key decoded successfully and needs no password phrase.
    Ready,
    /// The key is validly encrypted and requires a password phrase.
    PassphraseRequired,
}

/// Error returned while inspecting or validating an imported private key.
#[derive(Debug)]
pub enum PrivateKeyFileError {
    /// The key file could not be read from this path.
    Read {
        /// Path that failed to load.
        path: PathBuf,
        /// Platform IO error without any secret material.
        error: String,
    },
    /// The file contents are not a private key supported by the SSH backend.
    Decode(String),
}

impl fmt::Display for PrivateKeyFileError {
    /// Format a diagnostic without ever including key contents or a phrase.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => {
                write!(f, "failed to read private key {}: {error}", path.display())
            }
            Self::Decode(error) => write!(f, "failed to decode private key: {error}"),
        }
    }
}

impl std::error::Error for PrivateKeyFileError {}

// ---------------------------------------------------------------------------
// Private key decoding
// ---------------------------------------------------------------------------

pub(crate) fn decode_private_key(
    key_str: &str,
    passphrase: Option<&str>,
) -> Result<KeyPair, Box<dyn std::error::Error + Send + Sync>> {
    // Try PEM-encoded key first (OpenSSH format: "-----BEGIN OPENSSH PRIVATE KEY-----")
    if key_str.contains("BEGIN") {
        let pair = russh::keys::decode_secret_key(key_str, passphrase)?;
        return Ok(pair);
    }

    // Otherwise treat as a raw key file path or content — try as file path first
    if let Ok(content) = std::fs::read_to_string(key_str) {
        let pair = russh::keys::decode_secret_key(&content, passphrase)?;
        return Ok(pair);
    }

    Err("cannot decode private key: not a valid PEM key or file path".into())
}

/// Inspect a private-key path using the same decoder as real SSH connections.
///
/// No key contents are returned or logged. An encrypted key is reported as a
/// distinct status so the import preview can request its password phrase.
pub fn inspect_private_key_file(
    path: impl AsRef<Path>,
) -> Result<PrivateKeyFileStatus, PrivateKeyFileError> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path).map_err(|error| PrivateKeyFileError::Read {
        path: path.to_path_buf(),
        error: error.to_string(),
    })?;
    match russh::keys::decode_secret_key(&content, None) {
        Ok(_) => Ok(PrivateKeyFileStatus::Ready),
        Err(russh::keys::Error::KeyIsEncrypted) => Ok(PrivateKeyFileStatus::PassphraseRequired),
        Err(error) => Err(PrivateKeyFileError::Decode(error.to_string())),
    }
}

/// Validate an encrypted private-key file against a user-supplied phrase.
///
/// The phrase is borrowed only for the decode call and is never logged. The
/// decoded key is dropped immediately; persistence remains the caller's job.
pub fn validate_private_key_file(
    path: impl AsRef<Path>,
    passphrase: &str,
) -> Result<(), PrivateKeyFileError> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path).map_err(|error| PrivateKeyFileError::Read {
        path: path.to_path_buf(),
        error: error.to_string(),
    })?;
    russh::keys::decode_secret_key(&content, Some(passphrase))
        .map(|_| ())
        .map_err(|error| PrivateKeyFileError::Decode(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_that_is_neither_pem_nor_path_fails() {
        let err = decode_private_key("definitely-not-a-key", None)
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("cannot decode private key"));
    }

    #[test]
    fn pem_marker_with_invalid_body_fails() {
        // Contains "BEGIN" so it takes the PEM path — and must surface the
        // decode error rather than falling through to the file-path branch.
        let bogus =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nnot base64!!\n-----END OPENSSH PRIVATE KEY-----";
        assert!(decode_private_key(bogus, None).is_err());
    }

    #[test]
    fn path_to_file_with_invalid_contents_fails() {
        let path =
            std::env::temp_dir().join(format!("crabport-keys-test-{}.txt", std::process::id()));
        std::fs::write(&path, "this is not a private key").unwrap();
        assert!(decode_private_key(path.to_str().unwrap(), None).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn encrypted_key_requires_and_validates_passphrase() {
        const ENCRYPTED_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\n\
J2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\n\
KpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\n\
FkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\n\
e+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n\
7/wNsnDM0T7nLv/Q==\n\
-----END OPENSSH PRIVATE KEY-----";
        let path = std::env::temp_dir().join(format!(
            "crabport-encrypted-key-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, ENCRYPTED_KEY).unwrap();

        assert_eq!(
            inspect_private_key_file(&path).unwrap(),
            PrivateKeyFileStatus::PassphraseRequired
        );
        assert!(validate_private_key_file(&path, "wrong").is_err());
        assert!(validate_private_key_file(&path, "blabla").is_ok());
        let _ = std::fs::remove_file(path);
    }
}
