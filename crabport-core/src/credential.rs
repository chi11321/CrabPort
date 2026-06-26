use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostEntry {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credential_id: Option<i64>,
    pub kind: HostKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKind {
    Ssh,
    Telnet,
    Serial,
}

impl HostKind {
    pub fn default_port(&self) -> u16 {
        match self {
            HostKind::Ssh => 22,
            HostKind::Telnet => 23,
            HostKind::Serial => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Credential
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub id: i64,
    pub name: String,
    pub kind: CredentialKind,
    /// Anonymous credentials are auto-created from the connection form
    /// and hidden from the credentials list.
    #[serde(default)]
    pub anonymous: bool,
    /// For Password kind: the password. For Certificate kind: the passphrase.
    /// Stored encrypted in SQLite; decrypted only in memory.
    pub secret: String,
    /// Certificate-only fields (empty strings when not applicable).
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub certificate: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    Password,
    Certificate,
}
