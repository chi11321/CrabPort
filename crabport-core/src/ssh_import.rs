//! OpenSSH user-config discovery and import candidate extraction.
//!
//! Parsing and OpenSSH's top-down "first value wins" merge behavior are
//! delegated to `ssh2-config`. This module owns only CrabPort-specific policy:
//! concrete alias enumeration, supported-field mapping, path token expansion,
//! and explicit compatibility diagnostics for the import preview.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use ssh2_config::{HostParams, ParseRule, SshConfig};

/// One concrete SSH connection that can be shown in the import preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshImportCandidate {
    /// Concrete `Host` alias, preserved exactly for display and persistence.
    pub name: String,
    /// Resolved remote address (`HostName`, or the alias when omitted).
    pub host: String,
    /// Resolved SSH port, defaulting to 22.
    pub port: u16,
    /// Resolved remote user, defaulting to the current local account.
    pub username: String,
    /// Resolved identity files. Exactly one is required for import.
    pub identity_files: Vec<PathBuf>,
    /// Ordered `ProxyJump` aliases, from outermost to innermost hop.
    pub proxy_jump: Vec<String>,
    /// Non-blocking differences from the source OpenSSH configuration.
    pub notices: Vec<SshImportNotice>,
    /// Conditions that make this candidate unsafe or impossible to import.
    pub blockers: Vec<SshImportBlocker>,
}

impl SshImportCandidate {
    /// Return whether the candidate can be selected before dependency checks.
    pub fn is_importable(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Return the sole usable identity path when the candidate is importable.
    pub fn identity_file(&self) -> Option<&Path> {
        (self.identity_files.len() == 1).then(|| self.identity_files[0].as_path())
    }
}

/// A non-blocking difference displayed in the import preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshImportNotice {
    /// `HostName` was absent, so the concrete alias became the address.
    InferredHostName,
    /// `User` was absent, so the current local account was used.
    InferredUsername,
    /// Supported connection data was retained while these extras were omitted.
    IgnoredDirectives(Vec<String>),
}

/// A validation condition that prevents a candidate from being selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshImportBlocker {
    /// No explicit `IdentityFile` was resolved for this alias.
    MissingIdentityFile,
    /// More than one identity file was resolved and CrabPort cannot try a list.
    MultipleIdentityFiles(Vec<PathBuf>),
    /// The resolved identity path does not point to a regular file.
    IdentityFileMissing(PathBuf),
    /// The identity path contains a token CrabPort cannot resolve safely.
    UnsupportedIdentityToken(String),
    /// The jump specification is not a plain concrete alias.
    UnsupportedProxyJump(String),
    /// These directives change transport or authentication semantics.
    UnsupportedDirectives(Vec<String>),
}

/// Parsed import candidates plus source-level diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshImportScan {
    /// Absolute path of the root OpenSSH user configuration.
    pub config_path: PathBuf,
    /// Concrete aliases in source order, after case-insensitive deduplication.
    pub candidates: Vec<SshImportCandidate>,
    /// Wildcard or negated patterns intentionally excluded from the preview.
    pub skipped_patterns: Vec<String>,
}

/// One fully validated host mutation passed to the transactional store API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshImportRecord {
    /// Alias stored as the CrabPort connection name.
    pub name: String,
    /// Resolved remote address.
    pub host: String,
    /// Resolved SSH port.
    pub port: u16,
    /// Resolved remote username.
    pub username: String,
    /// Sole validated private-key file path.
    pub identity_file: PathBuf,
    /// Validated password phrase, empty for unencrypted keys.
    pub passphrase: String,
    /// Existing host row to update, or `None` to create a row.
    pub existing_host_id: Option<i64>,
    /// Immediate jump-host alias after expanding a `ProxyJump` chain.
    pub jump_host_name: Option<String>,
}

/// Counts returned after a successful atomic SSH import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SshImportSummary {
    /// Number of new host rows created.
    pub created: usize,
    /// Number of existing host rows updated.
    pub updated: usize,
}

/// Errors raised before a usable import preview can be built.
#[derive(Debug)]
pub enum SshImportError {
    /// The platform home directory could not be resolved.
    HomeDirectoryUnavailable,
    /// The default or requested SSH config does not exist.
    ConfigNotFound(PathBuf),
    /// The config or one of its includes could not be read.
    Io(String),
    /// OpenSSH syntax could not be parsed safely.
    Parse(String),
    /// No local username was available for OpenSSH's default `User` behavior.
    LocalUsernameUnavailable,
}

impl fmt::Display for SshImportError {
    /// Format a diagnostic suitable for logs and the import error notification.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeDirectoryUnavailable => write!(f, "cannot determine the home directory"),
            Self::ConfigNotFound(path) => {
                write!(f, "SSH config was not found at {}", path.display())
            }
            Self::Io(error) => write!(f, "failed to read SSH config: {error}"),
            Self::Parse(error) => write!(f, "failed to parse SSH config: {error}"),
            Self::LocalUsernameUnavailable => {
                write!(f, "cannot determine the current local username")
            }
        }
    }
}

impl std::error::Error for SshImportError {}

/// Return the cross-platform default OpenSSH user configuration path.
pub fn default_ssh_config_path() -> Result<PathBuf, SshImportError> {
    dirs::home_dir()
        .map(|home| home.join(".ssh").join("config"))
        .ok_or(SshImportError::HomeDirectoryUnavailable)
}

/// Scan the default OpenSSH user configuration for importable aliases.
pub fn scan_default_ssh_config() -> Result<SshImportScan, SshImportError> {
    let config_path = default_ssh_config_path()?;
    let local_username = current_local_username()?;
    scan_ssh_config(&config_path, &local_username)
}

/// Parse `config_path` and build CrabPort import candidates.
///
/// This path-taking variant is public so tests and future callers can scan an
/// explicitly known OpenSSH config without mutating process environment.
pub fn scan_ssh_config(
    config_path: &Path,
    local_username: &str,
) -> Result<SshImportScan, SshImportError> {
    if !config_path.is_file() {
        return Err(SshImportError::ConfigNotFound(config_path.to_path_buf()));
    }
    if local_username.trim().is_empty() {
        return Err(SshImportError::LocalUsernameUnavailable);
    }

    tracing::info!("ssh import: scanning {}", config_path.display());
    let file = File::open(config_path).map_err(|error| SshImportError::Io(error.to_string()))?;
    let mut reader = BufReader::new(file);
    let rules = ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS;
    let config = SshConfig::default()
        .parse(&mut reader, rules)
        .map_err(|error| SshImportError::Parse(error.to_string()))?;

    let home = dirs::home_dir().ok_or(SshImportError::HomeDirectoryUnavailable)?;
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let mut candidates = Vec::new();
    let mut skipped_patterns = Vec::new();
    let mut seen_aliases = HashSet::new();

    // Enumerate only literal positive clauses; the parser's query operation
    // still applies wildcard/global blocks to each resulting concrete alias.
    for host_block in config.get_hosts().iter().skip(1) {
        for clause in &host_block.pattern {
            let alias = clause.pattern.trim();
            if clause.negated || !is_concrete_alias(alias) {
                skipped_patterns.push(clause.to_string());
                continue;
            }
            let normalized = normalize_alias(alias);
            if !seen_aliases.insert(normalized) {
                continue;
            }
            let params = config.query(alias);
            candidates.push(build_candidate(
                alias,
                params,
                config_dir,
                &home,
                local_username,
            ));
        }
    }

    tracing::info!(
        "ssh import: scan complete ({} candidates, {} skipped patterns)",
        candidates.len(),
        skipped_patterns.len()
    );
    Ok(SshImportScan {
        config_path: config_path.to_path_buf(),
        candidates,
        skipped_patterns,
    })
}

/// Build one candidate from the fully merged parameters for a concrete alias.
fn build_candidate(
    alias: &str,
    params: HostParams,
    config_dir: &Path,
    home: &Path,
    local_username: &str,
) -> SshImportCandidate {
    let mut notices = Vec::new();
    let mut blockers = Vec::new();
    let host = params.host_name.clone().unwrap_or_else(|| {
        notices.push(SshImportNotice::InferredHostName);
        alias.to_string()
    });
    let username = params.user.clone().unwrap_or_else(|| {
        notices.push(SshImportNotice::InferredUsername);
        local_username.to_string()
    });

    let mut identity_files = Vec::new();
    for path in params.identity_file.clone().unwrap_or_default() {
        match expand_identity_path(&path, config_dir, home, &host, &username, local_username) {
            Ok(path) => identity_files.push(path),
            Err(token) => blockers.push(SshImportBlocker::UnsupportedIdentityToken(token)),
        }
    }
    match identity_files.as_slice() {
        [] if blockers.is_empty() => blockers.push(SshImportBlocker::MissingIdentityFile),
        [path] if !path.is_file() => {
            blockers.push(SshImportBlocker::IdentityFileMissing(path.clone()))
        }
        [_, _, ..] => blockers.push(SshImportBlocker::MultipleIdentityFiles(
            identity_files.clone(),
        )),
        _ => {}
    }

    let mut proxy_jump = Vec::new();
    for jump in params.proxy_jump.clone().unwrap_or_default() {
        let jump = jump.trim();
        if jump.eq_ignore_ascii_case("none") {
            continue;
        }
        if !is_concrete_alias(jump)
            || jump.contains('@')
            || jump.contains(':')
            || jump.contains('/')
        {
            blockers.push(SshImportBlocker::UnsupportedProxyJump(jump.to_string()));
        } else {
            proxy_jump.push(jump.to_string());
        }
    }

    let (critical, ignored) = classify_directives(&params);
    if !critical.is_empty() {
        blockers.push(SshImportBlocker::UnsupportedDirectives(critical));
    }
    if !ignored.is_empty() {
        notices.push(SshImportNotice::IgnoredDirectives(ignored));
    }

    SshImportCandidate {
        name: alias.to_string(),
        host,
        port: params.port.unwrap_or(22),
        username,
        identity_files,
        proxy_jump,
        notices,
        blockers,
    }
}

/// Split parsed-but-unmapped directives into blocking and informational sets.
fn classify_directives(params: &HostParams) -> (Vec<String>, Vec<String>) {
    const CRITICAL: &[&str] = &[
        "certificatefile",
        "identityagent",
        "pkcs11provider",
        "proxycommand",
        "securitykeyprovider",
    ];
    let mut critical = BTreeSet::new();
    let mut ignored = BTreeSet::new();

    if params.certificate_file.is_some() {
        critical.insert("CertificateFile".to_string());
    }
    for field in params
        .ignored_fields
        .keys()
        .chain(params.unsupported_fields.keys())
    {
        let normalized = field.to_ascii_lowercase();
        if CRITICAL.contains(&normalized.as_str()) {
            critical.insert(field.to_string());
        } else {
            ignored.insert(field.to_string());
        }
    }

    // These parsed fields are valid OpenSSH behavior but have no equivalent in
    // a CrabPort host row. Surface them instead of silently implying parity.
    if params.forward_agent.is_some() {
        ignored.insert("ForwardAgent".to_string());
    }
    if params.remote_forward.is_some() {
        ignored.insert("RemoteForward".to_string());
    }
    if params.server_alive_interval.is_some() {
        ignored.insert("ServerAliveInterval".to_string());
    }
    if params.tcp_keep_alive.is_some() {
        ignored.insert("TCPKeepAlive".to_string());
    }

    (
        critical.into_iter().collect(),
        ignored.into_iter().collect(),
    )
}

/// Expand the deterministic OpenSSH tokens supported by the import contract.
fn expand_identity_path(
    path: &Path,
    config_dir: &Path,
    home: &Path,
    remote_host: &str,
    remote_user: &str,
    local_user: &str,
) -> Result<PathBuf, String> {
    let raw = path.to_string_lossy();
    let mut expanded = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            expanded.push(ch);
            continue;
        }
        let Some(token) = chars.next() else {
            return Err("%".to_string());
        };
        match token {
            '%' => expanded.push('%'),
            'd' => expanded.push_str(&home.to_string_lossy()),
            'h' => expanded.push_str(remote_host),
            'r' => expanded.push_str(remote_user),
            'u' => expanded.push_str(local_user),
            other => return Err(format!("%{other}")),
        }
    }

    // OpenSSH expands a leading home marker before interpreting the path.
    let expanded = if let Some(relative) = expanded
        .strip_prefix("~/")
        .or_else(|| expanded.strip_prefix("~\\"))
    {
        home.join(relative)
    } else {
        PathBuf::from(expanded)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(config_dir.join(expanded))
    }
}

/// Return true only for a literal, positive alias that can become one row.
fn is_concrete_alias(alias: &str) -> bool {
    !alias.is_empty() && !alias.starts_with('!') && !alias.contains('*') && !alias.contains('?')
}

/// Normalize an alias for case-insensitive conflict and deduplication checks.
pub fn normalize_alias(alias: &str) -> String {
    alias.trim().to_ascii_lowercase()
}

/// Resolve the current local username without introducing platform-only APIs.
fn current_local_username() -> Result<String, SshImportError> {
    ["USERNAME", "USER"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|name| !name.trim().is_empty())
        .ok_or(SshImportError::LocalUsernameUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temporary SSH config fixture removed after every test.
    struct TempConfig {
        /// Root fixture directory.
        dir: PathBuf,
        /// Root config file path passed to the scanner.
        path: PathBuf,
    }

    impl TempConfig {
        /// Create a unique config fixture with `contents`.
        fn new(tag: &str, contents: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("crabport-ssh-import-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create fixture directory");
            let path = dir.join("config");
            std::fs::write(&path, contents).expect("write fixture config");
            Self { dir, path }
        }
    }

    impl Drop for TempConfig {
        /// Remove all fixture files after the test.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn scans_concrete_aliases_with_defaults_and_skips_patterns() {
        let fixture = TempConfig::new(
            "aliases",
            r#"
Host *
    User deploy
    Port 2200

Host prod-a prod-b *.internal !blocked
    HostName edge.example.com
    IdentityFile key.pem
"#,
        );
        std::fs::write(fixture.dir.join("key.pem"), "key").unwrap();

        let scan = scan_ssh_config(&fixture.path, "local-user").unwrap();
        assert_eq!(
            scan.candidates
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            ["prod-a", "prod-b"]
        );
        assert_eq!(scan.candidates[0].host, "edge.example.com");
        assert_eq!(scan.candidates[0].username, "deploy");
        assert_eq!(scan.candidates[0].port, 2200);
        let expected_key = fixture.dir.join("key.pem");
        assert_eq!(
            scan.candidates[0].identity_file(),
            Some(expected_key.as_path())
        );
        assert_eq!(scan.skipped_patterns, ["*", "*.internal", "!blocked"]);
    }

    #[test]
    fn infers_host_and_user_and_expands_identity_tokens() {
        let fixture = TempConfig::new(
            "tokens",
            r#"
Host app
    IdentityFile keys/%h-%r-%u
"#,
        );
        let key_path = fixture.dir.join("keys/app-local-user-local-user");
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        std::fs::write(&key_path, "key").unwrap();

        let scan = scan_ssh_config(&fixture.path, "local-user").unwrap();
        let candidate = &scan.candidates[0];
        assert_eq!(candidate.host, "app");
        assert_eq!(candidate.username, "local-user");
        assert_eq!(candidate.identity_file(), Some(key_path.as_path()));
        assert!(
            candidate
                .notices
                .contains(&SshImportNotice::InferredHostName)
        );
        assert!(
            candidate
                .notices
                .contains(&SshImportNotice::InferredUsername)
        );
    }

    #[test]
    fn blocks_missing_multiple_and_critical_auth_configuration() {
        let fixture = TempConfig::new(
            "blocked",
            r#"
Host missing
    HostName missing.example.com

Host multiple
    IdentityFile one
    IdentityFile two

Host agent-only
    IdentityFile key
    IdentityAgent ~/.ssh/agent.sock
"#,
        );

        let scan = scan_ssh_config(&fixture.path, "local-user").unwrap();
        assert!(
            scan.candidates[0]
                .blockers
                .contains(&SshImportBlocker::MissingIdentityFile)
        );
        assert!(matches!(
            scan.candidates[1].blockers.as_slice(),
            [SshImportBlocker::MultipleIdentityFiles(_)]
        ));
        assert!(scan.candidates[2].blockers.iter().any(|blocker| matches!(
            blocker,
            SshImportBlocker::UnsupportedDirectives(fields)
                if fields.iter().any(|field| field.eq_ignore_ascii_case("IdentityAgent"))
        )));
    }

    #[test]
    fn captures_plain_proxy_jump_chain_and_blocks_decorated_hops() {
        let fixture = TempConfig::new(
            "jumps",
            r#"
Host outer inner target decorated
    IdentityFile key

Host target
    ProxyJump outer,inner

Host decorated
    ProxyJump user@outer:2222
"#,
        );
        std::fs::write(fixture.dir.join("key"), "key").unwrap();

        let scan = scan_ssh_config(&fixture.path, "local-user").unwrap();
        let target = scan.candidates.iter().find(|c| c.name == "target").unwrap();
        assert_eq!(target.proxy_jump, ["outer", "inner"]);
        let decorated = scan
            .candidates
            .iter()
            .find(|c| c.name == "decorated")
            .unwrap();
        assert!(matches!(
            decorated.blockers.as_slice(),
            [SshImportBlocker::UnsupportedProxyJump(_)]
        ));
    }

    #[test]
    fn recursively_scans_included_host_blocks() {
        let fixture = TempConfig::new("include", "");
        let included = fixture.dir.join("included.conf");
        std::fs::write(
            &included,
            "Host included\n    HostName included.example.com\n    IdentityFile key\n",
        )
        .unwrap();
        let ssh_dir = dirs::home_dir().unwrap().join(".ssh");
        let include_path = relative_path(&ssh_dir, &included).unwrap();
        std::fs::write(
            &fixture.path,
            format!("Include {}\n", include_path.display()),
        )
        .unwrap();
        std::fs::write(fixture.dir.join("key"), "key").unwrap();

        let scan = scan_ssh_config(&fixture.path, "local-user").unwrap();
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].name, "included");
        assert_eq!(scan.candidates[0].host, "included.example.com");
    }

    #[test]
    fn expands_tilde_identity_path_to_home_directory() {
        // `Path::is_absolute()` is platform-dependent, so the fixture must
        // mirror the host it runs on rather than hard-coding a Windows drive.
        let home = if cfg!(windows) {
            PathBuf::from("C:/Users/example")
        } else {
            PathBuf::from("/home/example")
        };
        let config_dir = if cfg!(windows) {
            PathBuf::from("C:/config")
        } else {
            PathBuf::from("/etc/ssh")
        };
        let path = expand_identity_path(
            Path::new("~/.ssh/id_ed25519"),
            &config_dir,
            &home,
            "host",
            "remote",
            "local",
        )
        .unwrap();
        assert_eq!(path, home.join(".ssh/id_ed25519"));
    }

    /// Build a relative path without adding a test-only dependency.
    fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
        let base: Vec<_> = base.components().collect();
        let target: Vec<_> = target.components().collect();
        let common = base
            .iter()
            .zip(&target)
            .take_while(|(left, right)| left == right)
            .count();
        if common == 0 {
            return None;
        }
        let mut relative = PathBuf::new();
        for _ in common..base.len() {
            relative.push("..");
        }
        for component in &target[common..] {
            relative.push(component.as_os_str());
        }
        Some(relative)
    }

    #[test]
    fn alias_normalization_is_trimmed_and_ascii_case_insensitive() {
        assert_eq!(normalize_alias(" PROD "), "prod");
    }
}
