//! Atomic persistence for validated OpenSSH import records.

use std::collections::{HashMap, HashSet};

use rusqlite::{TransactionBehavior, params};

use crate::ssh_import::{SshImportRecord, SshImportSummary, normalize_alias};
use crate::store::StoreError;

use super::Store;

/// Encrypted credential material prepared before the database transaction.
struct EncryptedImportCredential {
    /// AES-GCM encrypted private-key path.
    private_key: Vec<u8>,
    /// AES-GCM encrypted password phrase, empty for unencrypted keys.
    passphrase: Vec<u8>,
}

impl Store {
    /// Persist all SSH import records in one immediate SQLite transaction.
    ///
    /// New credentials are created per host so deleting or editing one
    /// imported connection cannot affect another connection that references
    /// the same key path. Existing CrabPort-only host fields are preserved on
    /// update. Jump-host ids are linked only after every selected row exists.
    pub fn import_ssh_hosts(
        &mut self,
        records: &[SshImportRecord],
    ) -> Result<SshImportSummary, StoreError> {
        validate_records(records)?;

        // Encrypt outside the transaction so crypto failures cannot hold a
        // database write lock and never leave a partially prepared batch.
        let encrypted = records
            .iter()
            .map(|record| {
                Ok(EncryptedImportCredential {
                    private_key: self.encrypt_field(&record.identity_file.to_string_lossy())?,
                    passphrase: self.encrypt_field(&record.passphrase)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;

        tracing::info!(
            "ssh import: beginning transaction for {} records",
            records.len()
        );
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| StoreError::Db(error.to_string()))?;

        let mut aliases = load_existing_aliases(&transaction)?;
        let mut imported_ids = HashMap::new();
        let mut replaced_credentials = Vec::new();
        let mut summary = SshImportSummary::default();

        // First pass: create credentials and insert/update every host without
        // changing jump links. This guarantees all selected aliases have ids.
        for (record, encrypted_credential) in records.iter().zip(&encrypted) {
            let credential_id = insert_credential(&transaction, record, encrypted_credential)?;
            let host_id = match record.existing_host_id {
                Some(host_id) => {
                    let old_credential = update_host(&transaction, host_id, credential_id, record)?;
                    if let Some(old_credential) = old_credential {
                        replaced_credentials.push(old_credential);
                    }
                    summary.updated += 1;
                    host_id
                }
                None => {
                    summary.created += 1;
                    insert_host(&transaction, credential_id, record)?
                }
            };
            let normalized = normalize_alias(&record.name);
            imported_ids.insert(normalized.clone(), host_id);
            aliases.insert(
                normalized,
                vec![AliasTarget {
                    id: host_id,
                    is_ssh: true,
                }],
            );
        }

        // Second pass: resolve aliases against the now-complete row set and
        // write direct jump ids. Ambiguous existing names abort the batch.
        for record in records {
            let host_id = *imported_ids
                .get(&normalize_alias(&record.name))
                .ok_or_else(|| StoreError::Db("imported host id was not recorded".into()))?;
            let jump_host_id = match record.jump_host_name.as_deref() {
                Some(jump_name) => Some(resolve_unique_alias(&aliases, jump_name)?),
                None => None,
            };
            if jump_host_id == Some(host_id) {
                tracing::warn!("ssh import: direct jump cycle rejected for {}", record.name);
                return Err(StoreError::Db(format!(
                    "SSH import jump cycle references {} directly",
                    record.name
                )));
            }
            transaction
                .execute(
                    "UPDATE hosts SET jump_host_id = ?1 WHERE id = ?2",
                    params![jump_host_id, host_id],
                )
                .map_err(|error| StoreError::Db(error.to_string()))?;
        }

        // Existing jump links can combine with imported links into a longer
        // cycle. Validate only the subgraph reachable from the hosts this
        // import touched, so pre-existing unrelated cycle elsewhere in the DB
        // does not block (and get blamed on) this import.
        let imported_start_ids: HashSet<i64> = imported_ids.values().copied().collect();
        validate_jump_graph(&transaction, &imported_start_ids)?;

        // Remove credentials replaced by updates only when no other host still
        // references them. Shared/manual credentials are deliberately kept.
        for credential_id in replaced_credentials {
            transaction
                .execute(
                    "DELETE FROM credentials WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM hosts WHERE credential_id = ?1)",
                    params![credential_id],
                )
                .map_err(|error| StoreError::Db(error.to_string()))?;
        }

        transaction
            .commit()
            .map_err(|error| StoreError::Db(error.to_string()))?;
        tracing::info!(
            "ssh import: transaction committed ({} created, {} updated)",
            summary.created,
            summary.updated
        );
        Ok(summary)
    }
}

/// Reject duplicate or structurally invalid records before encryption begins.
fn validate_records(records: &[SshImportRecord]) -> Result<(), StoreError> {
    let mut names = HashSet::new();
    let mut update_ids = HashSet::new();
    for record in records {
        let name = normalize_alias(&record.name);
        if name.is_empty() {
            return Err(StoreError::Db("SSH import contains an empty alias".into()));
        }
        if !names.insert(name) {
            return Err(StoreError::Db(format!(
                "SSH import contains duplicate alias {}",
                record.name
            )));
        }
        if let Some(host_id) = record.existing_host_id
            && !update_ids.insert(host_id)
        {
            return Err(StoreError::Db(format!(
                "SSH import updates host {host_id} more than once"
            )));
        }
    }
    Ok(())
}

/// Load existing host ids grouped by normalized display name.
fn load_existing_aliases(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<HashMap<String, Vec<AliasTarget>>, StoreError> {
    let mut statement = transaction
        .prepare("SELECT id, name, kind FROM hosts ORDER BY id")
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let mut aliases: HashMap<String, Vec<AliasTarget>> = HashMap::new();
    for row in rows {
        let (id, name, kind) = row.map_err(|error| StoreError::Db(error.to_string()))?;
        aliases
            .entry(normalize_alias(&name))
            .or_default()
            .push(AliasTarget {
                id,
                is_ssh: kind == "Ssh",
            });
    }
    Ok(aliases)
}

/// Existing or newly imported host resolved by a normalized alias.
#[derive(Clone, Copy)]
struct AliasTarget {
    /// Host row id referenced by `jump_host_id`.
    id: i64,
    /// Whether the row can serve as an SSH jump host.
    is_ssh: bool,
}

/// Insert one anonymous certificate credential and return its row id.
fn insert_credential(
    transaction: &rusqlite::Transaction<'_>,
    record: &SshImportRecord,
    encrypted: &EncryptedImportCredential,
) -> Result<i64, StoreError> {
    transaction
        .execute(
            "INSERT INTO credentials (name, kind, anonymous, secret, private_key, private_key_kind, public_key, certificate) VALUES (?1,'Certificate',1,?2,?3,'Path',X'',X'')",
            params![record.name, encrypted.passphrase, encrypted.private_key],
        )
        .map_err(|error| StoreError::Db(error.to_string()))?;
    Ok(transaction.last_insert_rowid())
}

/// Insert one SSH host with CrabPort-specific fields at their neutral defaults.
fn insert_host(
    transaction: &rusqlite::Transaction<'_>,
    credential_id: i64,
    record: &SshImportRecord,
) -> Result<i64, StoreError> {
    transaction
        .execute(
            "INSERT INTO hosts (name, host, port, username, credential_id, kind, last_login, favorite, proxy_id, group_id, startup_command, serial_baud_rate, serial_data_bits, serial_parity, serial_stop_bits, serial_flow_control, jump_host_id) VALUES (?1,?2,?3,?4,?5,'Ssh',NULL,0,NULL,NULL,'',NULL,NULL,NULL,NULL,NULL,NULL)",
            params![record.name, record.host, record.port, record.username, credential_id],
        )
        .map_err(|error| StoreError::Db(error.to_string()))?;
    Ok(transaction.last_insert_rowid())
}

/// Update SSH-owned columns and return the credential row that was replaced.
///
/// Aborts the transaction if the existing row is not an SSH host: updating a
/// Telnet/Serial host through the OpenSSH import flow would silently rewrite
/// its kind and overwrite host/port/username, leaving dead serial columns.
fn update_host(
    transaction: &rusqlite::Transaction<'_>,
    host_id: i64,
    credential_id: i64,
    record: &SshImportRecord,
) -> Result<Option<i64>, StoreError> {
    let (old_credential, old_kind) = transaction
        .query_row(
            "SELECT credential_id, kind FROM hosts WHERE id = ?1",
            params![host_id],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| StoreError::Db(error.to_string()))?;
    if old_kind != "Ssh" {
        return Err(StoreError::Db(format!(
            "SSH import host {host_id} is not an SSH host (kind = {old_kind})"
        )));
    }
    let changed = transaction
        .execute(
            "UPDATE hosts SET name=?1, host=?2, port=?3, username=?4, credential_id=?5, kind='Ssh' WHERE id=?6",
            params![record.name, record.host, record.port, record.username, credential_id, host_id],
        )
        .map_err(|error| StoreError::Db(error.to_string()))?;
    if changed != 1 {
        return Err(StoreError::Db(format!(
            "SSH import host {host_id} could not be updated"
        )));
    }
    Ok(old_credential)
}

/// Resolve an alias only when it maps to exactly one existing/imported host.
fn resolve_unique_alias(
    aliases: &HashMap<String, Vec<AliasTarget>>,
    alias: &str,
) -> Result<i64, StoreError> {
    match aliases.get(&normalize_alias(alias)).map(Vec::as_slice) {
        Some([target]) if target.is_ssh => Ok(target.id),
        Some([_]) => Err(StoreError::Db(format!(
            "SSH import jump alias {alias} is not an SSH host"
        ))),
        Some(ids) => Err(StoreError::Db(format!(
            "SSH import jump alias {alias} is ambiguous ({} matches)",
            ids.len()
        ))),
        None => Err(StoreError::Db(format!(
            "SSH import jump alias {alias} was not found"
        ))),
    }
}

/// Reject cycles in the subgraph reachable from `start_ids` after the import.
///
/// Only paths rooted at hosts the current import created or updated are
/// walked, so a pre-existing cycle elsewhere in the DB does not abort (and
/// get misattributed to) an unrelated import. The whole jump-host table is
/// still loaded once to build the adjacency, but only reachable nodes are
/// tested for cycles.
fn validate_jump_graph(
    transaction: &rusqlite::Transaction<'_>,
    start_ids: &HashSet<i64>,
) -> Result<(), StoreError> {
    if start_ids.is_empty() {
        return Ok(());
    }
    let mut statement = transaction
        .prepare("SELECT id, jump_host_id FROM hosts WHERE jump_host_id IS NOT NULL")
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let mut links: HashMap<i64, Option<i64>> = HashMap::new();
    for row in rows {
        let (host_id, jump_host_id) = row.map_err(|error| StoreError::Db(error.to_string()))?;
        links.insert(host_id, jump_host_id);
    }

    let mut completed = HashSet::new();
    for &start in start_ids {
        if completed.contains(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = Some(start);
        while let Some(host_id) = current {
            if let Some(cycle_start) = positions.get(&host_id).copied() {
                let cycle: Vec<String> = path[cycle_start..]
                    .iter()
                    .chain(std::iter::once(&host_id))
                    .map(ToString::to_string)
                    .collect();
                tracing::warn!("ssh import: jump cycle rejected: {}", cycle.join(" -> "));
                return Err(StoreError::Db(format!(
                    "SSH import creates a jump cycle: {}",
                    cycle.join(" -> ")
                )));
            }
            if completed.contains(&host_id) {
                break;
            }
            positions.insert(host_id, path.len());
            path.push(host_id);
            current = links.get(&host_id).copied().flatten();
        }
        completed.extend(path);
    }
    Ok(())
}
