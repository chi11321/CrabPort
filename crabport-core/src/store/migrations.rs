//! Schema migrations — declarative list + runner.
//!
//! Each migration is a plain `.sql` file under `store/migrations/`, embedded
//! into the binary via `include_str!` and registered in [`MIGRATIONS`] in
//! order. A migration's version is its 1-based position in the list, and the
//! latest schema version is simply `MIGRATIONS.len()` — so adding a migration
//! is one new file plus one array entry, with no version constant to keep in
//! sync.
//!
//! The applied version is tracked in a `schema_version` table (max value
//! wins). On open, [`run`] executes every migration whose version is greater
//! than the recorded one, then records the new latest.
//!
//! ## Error policy
//!
//! Each entry declares how a failure is handled:
//!
//! - [`OnError::Fail`] — abort opening the store. For migrations that later
//!   code depends on structurally (new tables, first-time columns).
//! - [`OnError::WarnAndContinue`] — log and move on. For best-effort
//!   `ALTER TABLE ... ADD COLUMN` migrations that may legitimately fail when
//!   the column already exists (e.g. a fresh DB whose base migration already
//!   included it).
//!
//! Migrations are currently pure SQL. If one ever needs Rust logic (data
//! backfill, re-encryption, …), widen the entry to hold a
//! `fn(&Connection) -> Result<(), StoreError>` instead of restructuring.

use rusqlite::{Connection, params};

use super::StoreError;

/// What to do when a migration's SQL fails.
#[derive(Clone, Copy, Debug)]
enum OnError {
    /// Abort opening the store — later code depends on this migration.
    Fail,
    /// Log a warning and continue — best-effort ALTERs whose column may
    /// already exist on a fresh database.
    WarnAndContinue,
}

/// One schema migration: a human-readable name (for logs), its SQL, and the
/// failure policy.
struct Migration {
    name: &'static str,
    sql: &'static str,
    on_error: OnError,
}

/// Every migration, in order. **Append only** — a migration's version is its
/// position here (1-based), so inserting or reordering entries would re-run
/// or skip migrations on existing databases.
static MIGRATIONS: &[Migration] = &[
    Migration {
        name: "initial",
        sql: include_str!("migrations/001_initial.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "hosts_last_login_favorite",
        sql: include_str!("migrations/002_hosts_last_login_favorite.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "command_history",
        sql: include_str!("migrations/003_command_history.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "snippets",
        sql: include_str!("migrations/004_snippets.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "command_history.updated_at",
        sql: include_str!("migrations/005_command_history_updated_at.sql"),
        on_error: OnError::WarnAndContinue,
    },
    Migration {
        name: "proxies",
        sql: include_str!("migrations/006_proxies.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "tunnels",
        sql: include_str!("migrations/007_tunnels.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "credentials.private_key_kind",
        sql: include_str!("migrations/008_credentials_private_key_kind.sql"),
        on_error: OnError::WarnAndContinue,
    },
    Migration {
        name: "groups",
        sql: include_str!("migrations/009_groups.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "groups.favorite",
        sql: include_str!("migrations/010_groups_favorite.sql"),
        on_error: OnError::Fail,
    },
    Migration {
        name: "hosts.startup_command",
        sql: include_str!("migrations/011_hosts_startup_command.sql"),
        on_error: OnError::WarnAndContinue,
    },
    Migration {
        name: "hosts serial config",
        sql: include_str!("migrations/012_hosts_serial_config.sql"),
        on_error: OnError::WarnAndContinue,
    },
    Migration {
        name: "hosts.jump_host_id",
        sql: include_str!("migrations/013_hosts_jump_host_id.sql"),
        on_error: OnError::WarnAndContinue,
    },
];

/// Bring the database up to the latest schema version.
pub(crate) fn run(db: &Connection) -> Result<(), StoreError> {
    // Ensure the schema_version tracking table exists.
    db.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
        .map_err(|e| StoreError::Db(e.to_string()))?;

    let current: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    for (idx, m) in MIGRATIONS.iter().enumerate() {
        let version = (idx + 1) as i64;
        if current >= version {
            continue;
        }
        if let Err(e) = db.execute_batch(m.sql) {
            match m.on_error {
                OnError::Fail => return Err(StoreError::Db(e.to_string())),
                OnError::WarnAndContinue => {
                    tracing::warn!("store: migration {version} ({}) failed: {e}", m.name);
                }
            }
        }
    }

    // Record the latest migration version.
    let latest = MIGRATIONS.len() as i64;
    if current < latest {
        db.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![latest],
        )
        .map_err(|e| StoreError::Db(e.to_string()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh DB runs every migration and lands on `MIGRATIONS.len()`;
    /// reopening an up-to-date DB is a no-op (no duplicate version rows).
    /// Also spot-checks that a late migration's column actually exists.
    #[test]
    fn fresh_db_migrates_to_latest_and_reopen_is_noop() {
        let dir = std::env::temp_dir().join(format!(
            "crabport-migrations-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        {
            let store = super::super::Store::open_at(dir.clone()).expect("fresh open");
            let version: i64 = store
                .db
                .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, MIGRATIONS.len() as i64);
            // Column from the last migration must exist (prepare fails if not).
            store
                .db
                .prepare("SELECT jump_host_id FROM hosts")
                .expect("migration 13 column missing");
        }

        {
            let store = super::super::Store::open_at(dir.clone()).expect("reopen");
            let rows: i64 = store
                .db
                .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1, "re-running migrations must not add version rows");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
