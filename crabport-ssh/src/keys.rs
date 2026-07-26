use russh::keys::key::KeyPair;

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
        let bogus = "-----BEGIN OPENSSH PRIVATE KEY-----\nnot base64!!\n-----END OPENSSH PRIVATE KEY-----";
        assert!(decode_private_key(bogus, None).is_err());
    }

    #[test]
    fn path_to_file_with_invalid_contents_fails() {
        let path = std::env::temp_dir().join(format!(
            "crabport-keys-test-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "this is not a private key").unwrap();
        assert!(decode_private_key(path.to_str().unwrap(), None).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
