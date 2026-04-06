//! Migration runner for `ModKit` modules.
//!
//! This module provides a secure migration execution system that:
//! - Executes module-provided migrations using a **per-module** migration history table.
//! - Does **not** expose raw database connections or `SQLx` pools to modules.
//! - Ensures deterministic, idempotent migration execution.
//!
//! # Per-Module Migration Tables
//!
//! Each module gets its own migration history table named `modkit_migrations__<prefix>__<hash8>`,
//! where `<hash8>` is an 8-character hex hash derived from the module prefix via `xxh3_64`.
//! This prevents conflicts between modules that might have similarly named migrations.
//!
//! Examples:
//! - Test prefix "_test" → `modkit_migrations___test__e5f6a7b8`
//!
//! # Security Model
//!
//! Modules only provide migration definitions via `MigrationTrait`. The runtime executes
//! them using its privileged connection. Modules never receive raw database access.

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DbErr, ExecResult, FromQueryResult, Statement,
    TransactionTrait,
};
use sea_orm_migration::MigrationTrait;
use std::collections::HashSet;
use thiserror::Error;
use tracing::{debug, info};
use xxhash_rust::xxh3::xxh3_64;

/// Errors that can occur during migration execution.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Failed to create the migration history table.
    #[error("failed to create migration table for module '{module}': {source}")]
    CreateTable { module: String, source: DbErr },

    /// Failed to query existing migrations.
    #[error("failed to query migration history for module '{module}': {source}")]
    QueryHistory { module: String, source: DbErr },

    /// A specific migration failed to execute.
    #[error("migration '{migration}' failed for module '{module}': {source}")]
    MigrationFailed {
        module: String,
        migration: String,
        source: DbErr,
    },

    /// Failed to record a migration in the history table.
    #[error("failed to record migration '{migration}' for module '{module}': {source}")]
    RecordFailed {
        module: String,
        migration: String,
        source: DbErr,
    },

    /// Duplicate migration name found in provided migrations list.
    #[error("duplicate migration name '{name}' for module '{module}'")]
    DuplicateMigrationName { module: String, name: String },
}

/// Result of a migration run.
#[derive(Debug, Clone)]
pub struct MigrationResult {
    /// Number of migrations that were applied.
    pub applied: usize,
    /// Number of migrations that were skipped (already applied).
    pub skipped: usize,
    /// Names of the migrations that were applied.
    pub applied_names: Vec<String>,
}

/// Internal model for querying migration history.
#[derive(Debug, FromQueryResult)]
struct MigrationRecord {
    version: String,
}

/// Sanitize a module name into a safe identifier fragment.
///
/// Rules:
/// - Allowed: `[a-zA-Z0-9_]`
/// - Everything else becomes `_` (DO NOT hard-fail on '.', '/', etc.)
fn sanitize_module_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => out.push(c),
            _ => out.push('_'),
        }
    }
    if out.is_empty() { "_".to_owned() } else { out }
}

/// Build the per-module migration table name.
///
/// Format: `modkit_migrations__<prefix>__<hash8>`
///
/// - `<prefix>` is a sanitized module name fragment
/// - `<hash8>` is a stable hash of the ORIGINAL module name
/// - Name is capped to Postgres 63-byte identifier limit (and kept short for all backends)
fn migration_table_name(module_name: &str) -> String {
    const PREFIX: &str = "modkit_migrations__";
    const SEP: &str = "__";
    const HASH_LEN: usize = 8;
    const PG_IDENT_MAX: usize = 63;

    let sanitized = sanitize_module_name(module_name);
    let hash = xxh3_64(module_name.as_bytes());
    let hash8 = format!("{hash:016x}")[..HASH_LEN].to_owned();

    // Reserve space for: PREFIX + prefix + SEP + hash8
    let reserved = PREFIX.len() + SEP.len() + HASH_LEN;
    let max_prefix_len = PG_IDENT_MAX.saturating_sub(reserved);
    let prefix_part = if max_prefix_len == 0 {
        String::new()
    } else if sanitized.len() > max_prefix_len {
        sanitized[..max_prefix_len].to_owned()
    } else {
        sanitized
    };

    format!("{PREFIX}{prefix_part}{SEP}{hash8}")
}

/// Create the migration history table for a module if it doesn't exist.
async fn ensure_migration_table(
    conn: &impl ConnectionTrait,
    table_name: &str,
    module_name: &str,
) -> Result<(), MigrationError> {
    let backend = conn.get_database_backend();

    let sql = match backend {
        DatabaseBackend::Postgres => format!(
            r#"
            CREATE TABLE IF NOT EXISTS "{table_name}" (
                version VARCHAR(255) PRIMARY KEY,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#
        ),
        DatabaseBackend::MySql => format!(
            r"
            CREATE TABLE IF NOT EXISTS `{table_name}` (
                version VARCHAR(255) PRIMARY KEY,
                applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "
        ),
        DatabaseBackend::Sqlite => format!(
            r#"
            CREATE TABLE IF NOT EXISTS "{table_name}" (
                version TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#
        ),
    };

    conn.execute(Statement::from_string(backend, sql))
        .await
        .map_err(|e| MigrationError::CreateTable {
            module: module_name.to_owned(),
            source: e,
        })?;

    Ok(())
}

/// Query all applied migrations for a module.
async fn get_applied_migrations(
    conn: &impl ConnectionTrait,
    table_name: &str,
    module_name: &str,
) -> Result<HashSet<String>, MigrationError> {
    let backend = conn.get_database_backend();

    let sql = match backend {
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            format!(r#"SELECT version FROM "{table_name}""#)
        }
        DatabaseBackend::MySql => format!(r"SELECT version FROM `{table_name}`"),
    };

    let records: Vec<MigrationRecord> =
        MigrationRecord::find_by_statement(Statement::from_string(backend, sql))
            .all(conn)
            .await
            .map_err(|e| MigrationError::QueryHistory {
                module: module_name.to_owned(),
                source: e,
            })?;

    Ok(records.into_iter().map(|r| r.version).collect())
}

/// Record a migration as applied.
async fn record_migration(
    conn: &impl ConnectionTrait,
    table_name: &str,
    module_name: &str,
    migration_name: &str,
) -> Result<ExecResult, MigrationError> {
    let backend = conn.get_database_backend();

    let sql = match backend {
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            format!(r#"INSERT INTO "{table_name}" (version) VALUES ($1)"#)
        }
        DatabaseBackend::MySql => format!(r"INSERT INTO `{table_name}` (version) VALUES (?)"),
    };

    conn.execute(Statement::from_sql_and_values(
        backend,
        &sql,
        [migration_name.into()],
    ))
    .await
    .map_err(|e| MigrationError::RecordFailed {
        module: module_name.to_owned(),
        migration: migration_name.to_owned(),
        source: e,
    })
}

/// Run migrations for a specific module using a `Db`.
///
/// This is the main entry point for the runtime to execute module migrations.
/// It uses the internal database connection from the handle.
///
/// # Arguments
///
/// * `db` - The secure database entrypoint (owned by the runtime).
/// * `module_name` - The name of the module (used for the migration table name).
/// * `migrations` - The list of migrations to run.
///
/// # Returns
///
/// Returns `Ok(MigrationResult)` with statistics about the migration run,
/// or an error if any migration fails.
///
/// # Example
///
/// ```ignore
/// use modkit_db::migration_runner::run_migrations_for_module;
///
/// let migrations: Vec<Box<dyn MigrationTrait>> = module.migrations();
/// let result = run_migrations_for_module(&db, "my_module", migrations).await?;
/// println!("Applied {} migrations", result.applied);
/// ```
///
/// # Errors
///
/// Returns `Err(MigrationError)` if the migration table cannot be created, the history
/// cannot be queried, or any migration fails.
pub async fn run_migrations_for_module(
    db: &crate::Db,
    module_name: &str,
    migrations: Vec<Box<dyn MigrationTrait>>,
) -> Result<MigrationResult, MigrationError> {
    let conn = db.sea_internal();
    run_module_migrations(&conn, module_name, migrations).await
}

/// Run migrations for a specific module (internal implementation).
///
/// This function:
/// 1. Creates a per-module migration table if it doesn't exist.
/// 2. Queries which migrations have already been applied.
/// 3. Sorts migrations by name for deterministic ordering.
/// 4. Executes pending migrations and records them.
///
/// # Arguments
///
/// * `conn` - The database connection (privileged, from the runtime).
/// * `module_name` - The name of the module (used for the migration table name).
/// * `migrations` - The list of migrations to run.
///
/// # Returns
///
/// Returns `Ok(MigrationResult)` with statistics about the migration run,
/// or an error if any migration fails.
async fn run_module_migrations<C>(
    conn: &C,
    module_name: &str,
    migrations: Vec<Box<dyn MigrationTrait>>,
) -> Result<MigrationResult, MigrationError>
where
    C: ConnectionTrait + TransactionTrait,
{
    if migrations.is_empty() {
        debug!(module = module_name, "No migrations to run");
        return Ok(MigrationResult {
            applied: 0,
            skipped: 0,
            applied_names: vec![],
        });
    }

    // Reject duplicate migration names early (security/correctness: deterministic + idempotent)
    let mut seen = HashSet::new();
    for m in &migrations {
        let n = m.name().to_owned();
        if !seen.insert(n.clone()) {
            return Err(MigrationError::DuplicateMigrationName {
                module: module_name.to_owned(),
                name: n,
            });
        }
    }

    // Get the per-module migration table name
    let table_name = migration_table_name(module_name);

    // Ensure the migration table exists
    ensure_migration_table(conn, &table_name, module_name).await?;

    // Get already-applied migrations
    let applied = get_applied_migrations(conn, &table_name, module_name).await?;

    // Sort migrations by name for deterministic ordering
    let mut sorted_migrations: Vec<_> = migrations.into_iter().collect();
    sorted_migrations.sort_by(|a, b| a.name().cmp(b.name()));

    let mut result = MigrationResult {
        applied: 0,
        skipped: 0,
        applied_names: vec![],
    };

    for migration in sorted_migrations {
        let name = migration.name().to_owned();

        if applied.contains(&name) {
            debug!(
                module = module_name,
                migration = %name,
                "Migration already applied, skipping"
            );
            result.skipped += 1;
            continue;
        }

        info!(
            module = module_name,
            migration = %name,
            "Applying migration"
        );

        // Best-effort atomicity:
        // Try to wrap `up()` + history record into an explicit transaction.
        // Note: Some backends (or specific DDL) may auto-commit; this is still best-effort.
        let txn = conn
            .begin()
            .await
            .map_err(|e| MigrationError::MigrationFailed {
                module: module_name.to_owned(),
                migration: name.clone(),
                source: e,
            })?;

        let manager = sea_orm_migration::SchemaManager::new(&txn);
        let res: Result<(), MigrationError> = (async {
            migration
                .up(&manager)
                .await
                .map_err(|e| MigrationError::MigrationFailed {
                    module: module_name.to_owned(),
                    migration: name.clone(),
                    source: e,
                })?;

            record_migration(&txn, &table_name, module_name, &name).await?;
            Ok(())
        })
        .await;

        match res {
            Ok(()) => {
                txn.commit()
                    .await
                    .map_err(|e| MigrationError::MigrationFailed {
                        module: module_name.to_owned(),
                        migration: name.clone(),
                        source: e,
                    })?;
            }
            Err(err) => {
                _ = txn.rollback().await;
                return Err(err);
            }
        }

        info!(
            module = module_name,
            migration = %name,
            "Migration applied successfully"
        );

        result.applied += 1;
        result.applied_names.push(name);
    }

    info!(
        module = module_name,
        applied = result.applied,
        skipped = result.skipped,
        "Migration run complete"
    );

    Ok(result)
}

/// Run migrations for testing purposes.
///
/// This is a convenience function for unit tests that don't need per-module
/// table separation. It calls `migration_table_name("_test")` which produces
/// a hashed table name like `modkit_migrations___test__<hash8>`.
///
/// # Arguments
///
/// * `db` - The database handle.
/// * `migrations` - The list of migrations to run.
///
/// # Returns
///
/// Returns `Ok(MigrationResult)` or an error if any migration fails.
///
/// # Errors
///
/// Returns `Err(MigrationError)` if the migration table cannot be created, the history
/// cannot be queried, or any migration fails.
///
/// # Example
///
/// ```ignore
/// use modkit_db::migration_runner::run_migrations_for_testing;
///
/// #[tokio::test]
/// async fn test_my_migrations() {
///     let db = setup_test_db().await;
///     let migrations = my_module::SettingsModule::default().migrations();
///     let result = run_migrations_for_testing(&db, migrations).await.unwrap();
///     assert_eq!(result.applied, 2);
/// }
/// ```
pub async fn run_migrations_for_testing(
    db: &crate::Db,
    migrations: Vec<Box<dyn MigrationTrait>>,
) -> Result<MigrationResult, MigrationError> {
    let conn = db.sea_internal();
    run_module_migrations(&conn, "_test", migrations).await
}

/// Check if migrations are pending for a module without applying them.
///
/// # Arguments
///
/// * `db` - The database handle.
/// * `module_name` - The name of the module.
/// * `migrations` - The list of migrations to check.
///
/// # Returns
///
/// Returns a list of migration names that have not been applied yet.
///
/// # Errors
///
/// Returns `Err(MigrationError)` if the migration history cannot be queried.
pub async fn get_pending_migrations(
    db: &crate::Db,
    module_name: &str,
    migrations: &[Box<dyn MigrationTrait>],
) -> Result<Vec<String>, MigrationError> {
    let conn = db.sea_internal();
    get_pending_migrations_internal(&conn, module_name, migrations).await
}

/// Internal implementation for checking pending migrations.
async fn get_pending_migrations_internal(
    conn: &impl ConnectionTrait,
    module_name: &str,
    migrations: &[Box<dyn MigrationTrait>],
) -> Result<Vec<String>, MigrationError> {
    if migrations.is_empty() {
        return Ok(vec![]);
    }

    let table_name = migration_table_name(module_name);

    // Check if table exists - if not, all migrations are pending.
    // Propagate DB errors rather than treating them as "table missing".
    let backend = conn.get_database_backend();
    let table_exists = match backend {
        DatabaseBackend::Postgres => {
            let sql = format!(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = '{table_name}')"
            );
            let row = conn
                .query_one(Statement::from_string(backend, sql))
                .await
                .map_err(|e| MigrationError::QueryHistory {
                    module: module_name.to_owned(),
                    source: e,
                })?;
            row.and_then(|r| r.try_get_by_index::<bool>(0).ok())
                .unwrap_or(false)
        }
        DatabaseBackend::MySql => {
            let sql = format!(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = '{table_name}'"
            );
            let row = conn
                .query_one(Statement::from_string(backend, sql))
                .await
                .map_err(|e| MigrationError::QueryHistory {
                    module: module_name.to_owned(),
                    source: e,
                })?;
            row.and_then(|r| r.try_get_by_index::<i64>(0).ok())
                .is_some_and(|c| c > 0)
        }
        DatabaseBackend::Sqlite => {
            let sql = format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table_name}'"
            );
            let row = conn
                .query_one(Statement::from_string(backend, sql))
                .await
                .map_err(|e| MigrationError::QueryHistory {
                    module: module_name.to_owned(),
                    source: e,
                })?;
            row.and_then(|r| r.try_get_by_index::<i32>(0).ok())
                .is_some_and(|c| c > 0)
        }
    };

    if !table_exists {
        return Ok(migrations.iter().map(|m| m.name().to_owned()).collect());
    }

    let applied = get_applied_migrations(conn, &table_name, module_name).await?;

    Ok(migrations
        .iter()
        .filter(|m| !applied.contains(m.name()))
        .map(|m| m.name().to_owned())
        .collect())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "migration_runner_tests.rs"]
mod tests;
