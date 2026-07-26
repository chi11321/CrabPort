//! Persistent `known_hosts` store for SSH host-key verification (TOFU).
//!
//! # File layout
//!
//! ```text
//! {data_dir}/crabport/known_hosts
//! ```
//!
//! Each line is tab-separated:
//!
//! ```text
//! <host>\t<port>\t<algo>\t<fingerprint>
//! ```
//!
//! - `host` is the hostname or IP literal as given by the user (no
//!   bracketing, no surrounding whitespace).
//! - `port` is the SSH port.
//! - `algo` is `russh::keys::key::PublicKey::name()`, e.g. `ssh-ed25519`.
//! - `fingerprint` is the SHA-256 base64 fingerprint from
//!   `russh::keys::key::PublicKey::fingerprint()`.
//!
//! The file is rewritten atomically on each successful new entry. It is
//! intentionally line-oriented and grep-friendly so it can be inspected
//! or edited by hand.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// A single known_hosts entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KnownHost {
    pub host: String,
    pub port: u16,
    pub algo: String,
    pub fingerprint: String,
}

impl KnownHost {
    /// Serialize to a single line (without trailing newline).
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.host, self.port, self.algo, self.fingerprint
        )
    }

    /// Parse a single line. Returns `None` on malformed input.
    pub fn from_line(line: &str) -> Option<Self> {
        let mut parts = line.split('\t');
        let host = parts.next()?.trim().to_string();
        let port: u16 = parts.next()?.trim().parse().ok()?;
        let algo = parts.next()?.trim().to_string();
        let fingerprint = parts.next()?.trim().to_string();
        if host.is_empty() || algo.is_empty() || fingerprint.is_empty() {
            return None;
        }
        Some(Self {
            host,
            port,
            algo,
            fingerprint,
        })
    }
}

/// Result of looking up a host key in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult {
    /// No entry for this `host:port` exists yet — the caller should ask
    /// the user whether to trust this key (TOFU).
    NotFound,
    /// An entry exists and its fingerprint matches the presented key.
    Matched,
    /// An entry exists but the fingerprint differs from the presented
    /// key — a likely man-in-the-middle attack. The caller MUST refuse
    /// to connect.
    Mismatched {
        expected_algo: String,
        expected_fingerprint: String,
    },
}

/// Persistent known_hosts store backed by a plain text file.
pub struct KnownHosts {
    path: PathBuf,
}

impl KnownHosts {
    /// Open (or lazily create) the store at the platform data directory.
    pub fn open() -> std::io::Result<Self> {
        let dir = default_data_dir()?;
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("known_hosts"),
        })
    }

    /// Open at an explicit path (mainly for tests).
    pub fn open_at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read all entries currently in the file.
    fn read_all(&self) -> std::io::Result<HashSet<KnownHost>> {
        let mut set = HashSet::new();
        let Ok(contents) = fs::read_to_string(&self.path) else {
            // Missing file == empty store.
            return Ok(set);
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(entry) = KnownHost::from_line(line) {
                set.insert(entry);
            }
        }
        Ok(set)
    }

    /// Look up the presented key for `host:port`.
    pub fn lookup(
        &self,
        host: &str,
        port: u16,
        algo: &str,
        fingerprint: &str,
    ) -> std::io::Result<LookupResult> {
        let all = self.read_all()?;
        // Match any entry for this host:port (we may have more than one
        // if the server rotated keys across algorithms).
        let mut found = false;
        for entry in &all {
            if entry.host == host && entry.port == port {
                found = true;
                if entry.algo == algo && entry.fingerprint == fingerprint {
                    return Ok(LookupResult::Matched);
                }
            }
        }
        if found {
            // We have an entry for this host:port but none matched the
            // presented (algo, fingerprint). Report the first stored one
            // as the "expected" value for diagnostics.
            let expected = all
                .iter()
                .find(|e| e.host == host && e.port == port)
                .expect("found == true implies an entry exists");
            Ok(LookupResult::Mismatched {
                expected_algo: expected.algo.clone(),
                expected_fingerprint: expected.fingerprint.clone(),
            })
        } else {
            Ok(LookupResult::NotFound)
        }
    }

    /// Persist a new trusted entry. Rewrites the file atomically.
    pub fn add(&self, entry: &KnownHost) -> std::io::Result<()> {
        let mut all = self.read_all()?;
        all.insert(entry.clone());

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            writeln!(
                f,
                "# CrabPort known_hosts — do not edit while the app is running"
            )?;
            // Sort for deterministic output.
            let mut lines: Vec<String> = all.iter().map(|e| e.to_line()).collect();
            lines.sort();
            for line in lines {
                writeln!(f, "{line}")?;
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn default_data_dir() -> std::io::Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "data dir"))?;
    Ok(base.join("crabport"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "crabport-known-hosts-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_file(&p);
        p
    }

    fn entry(host: &str, port: u16, algo: &str, fp: &str) -> KnownHost {
        KnownHost {
            host: host.into(),
            port,
            algo: algo.into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn line_roundtrip_and_malformed_lines() {
        let e = entry("example.com", 2222, "ssh-ed25519", "SHA256:abc/def+g=");
        assert_eq!(KnownHost::from_line(&e.to_line()), Some(e));

        // Malformed inputs are rejected, not mis-parsed.
        assert_eq!(KnownHost::from_line(""), None);
        assert_eq!(KnownHost::from_line("host-only"), None);
        assert_eq!(KnownHost::from_line("host\tnot-a-port\talgo\tfp"), None);
        assert_eq!(KnownHost::from_line("host\t22\talgo"), None); // missing fp
        assert_eq!(KnownHost::from_line("\t22\talgo\tfp"), None); // empty host
    }

    #[test]
    fn tofu_lookup_lifecycle() {
        let path = tmp_path("lifecycle");
        let kh = KnownHosts::open_at(path.clone());

        // Unknown host:port → NotFound (ask the user).
        assert_eq!(
            kh.lookup("example.com", 22, "ssh-ed25519", "SHA256:aaa").unwrap(),
            LookupResult::NotFound
        );

        // Trust it, then the same key matches.
        kh.add(&entry("example.com", 22, "ssh-ed25519", "SHA256:aaa"))
            .unwrap();
        assert_eq!(
            kh.lookup("example.com", 22, "ssh-ed25519", "SHA256:aaa").unwrap(),
            LookupResult::Matched
        );

        // Same host, different key → Mismatched with the stored expectation.
        match kh.lookup("example.com", 22, "ssh-ed25519", "SHA256:EVIL").unwrap() {
            LookupResult::Mismatched {
                expected_algo,
                expected_fingerprint,
            } => {
                assert_eq!(expected_algo, "ssh-ed25519");
                assert_eq!(expected_fingerprint, "SHA256:aaa");
            }
            other => panic!("expected Mismatched, got {other:?}"),
        }

        // Same host on a different port is a separate TOFU decision.
        assert_eq!(
            kh.lookup("example.com", 2222, "ssh-ed25519", "SHA256:aaa").unwrap(),
            LookupResult::NotFound
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn add_dedups_and_persists_comments_ignored() {
        let path = tmp_path("dedup");
        let kh = KnownHosts::open_at(path.clone());
        let e = entry("h", 22, "ssh-rsa", "SHA256:x");
        kh.add(&e).unwrap();
        kh.add(&e).unwrap(); // duplicate insert is a no-op

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents.lines().filter(|l| l.contains("SHA256:x")).count(),
            1,
            "duplicate entries must be deduped"
        );
        assert!(contents.starts_with('#'), "file keeps its header comment");

        // A fresh handle re-reads the same store (persistence) and the
        // header/comment lines don't break parsing.
        let kh2 = KnownHosts::open_at(path.clone());
        assert_eq!(
            kh2.lookup("h", 22, "ssh-rsa", "SHA256:x").unwrap(),
            LookupResult::Matched
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn multiple_algorithms_per_host_can_all_match() {
        let path = tmp_path("multi-algo");
        let kh = KnownHosts::open_at(path.clone());
        kh.add(&entry("h", 22, "ssh-ed25519", "SHA256:ed")).unwrap();
        kh.add(&entry("h", 22, "rsa-sha2-512", "SHA256:rsa")).unwrap();

        assert_eq!(
            kh.lookup("h", 22, "ssh-ed25519", "SHA256:ed").unwrap(),
            LookupResult::Matched
        );
        assert_eq!(
            kh.lookup("h", 22, "rsa-sha2-512", "SHA256:rsa").unwrap(),
            LookupResult::Matched
        );

        let _ = fs::remove_file(&path);
    }
}
