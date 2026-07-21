//! Shared pool/migration/startup-validation helpers used by both the
//! combined [`crate::PostgresClusterPlugin`] and the standalone
//! [`crate::PostgresLockPlugin`] builders (DESIGN.md §3.2, §3.5).

use cluster_sdk::ClusterError;
use sqlx::PgConnection;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use tracing::warn;

use crate::config::ReplicationMode;
use crate::pg_error::map_sqlx_error;

/// A [`PgPoolOptions`] with the `after_connect`/`before_acquire`/`after_release`
/// hooks wired (DESIGN.md §3.4) — callers still need to chain
/// `.max_connections(...)`, `.acquire_timeout(...)`, and `.connect(...)`.
///
/// `after_connect` pins every connection's `search_path` to `schema` so the
/// migrations' unqualified `CREATE TABLE`s land in the same schema the runtime
/// queries target with their `{schema}.cluster_*` qualifiers (PGR-L4). `schema`
/// must already have passed [`validate_schema`] — it is interpolated unquoted.
pub fn base_pool_options(schema: &str) -> PgPoolOptions {
    let schema = schema.to_owned();
    PgPoolOptions::new()
        .after_connect(move |conn, _meta| {
            let schema = schema.clone();
            Box::pin(async move {
                set_synchronous_commit(conn).await?;
                set_search_path(conn, &schema).await?;
                Ok(())
            })
        })
        .before_acquire(|conn, _meta| {
            Box::pin(async move {
                set_synchronous_commit(conn).await?;
                Ok(true)
            })
        })
        .after_release(|conn, _meta| Box::pin(release_session_advisory_locks(conn)))
}

/// Validates that `schema` is a simple, safe SQL identifier (PGR-L4). The schema
/// is interpolated **unquoted** into DDL (`CREATE SCHEMA`, `SET search_path`) and
/// into every runtime table identifier (`{schema}.cluster_*`), so restricting it
/// to the identifier charset keeps the migration schema and the query schema
/// resolving to the same object (an unquoted identifier is case-folded by
/// Postgres) and forecloses any injection via the config value.
///
/// # Errors
/// [`ClusterError::InvalidConfig`] if `schema` is empty, longer than Postgres's
/// 63-byte identifier limit, or contains anything other than ASCII letters,
/// digits, and underscores (and does not start with a digit).
pub fn validate_schema(schema: &str) -> Result<(), ClusterError> {
    let valid = !schema.is_empty()
        && schema.len() <= 63
        && schema
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && schema
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "schema {schema:?} must be a simple identifier (ASCII letters, digits, and \
                 underscores; not starting with a digit; at most 63 bytes) — it is interpolated \
                 unquoted into DDL and query identifiers"
            ),
        });
    }
    Ok(())
}

/// `SET search_path TO <schema>`, enforced on every connection via
/// `after_connect` (PGR-L4) so unqualified migration DDL creates its tables in
/// the configured schema. `schema` must have passed [`validate_schema`].
pub async fn set_search_path(conn: &mut PgConnection, schema: &str) -> Result<(), sqlx::Error> {
    sqlx::query(&format!("SET search_path TO {schema}"))
        .execute(conn)
        .await?;
    Ok(())
}

/// `CREATE SCHEMA IF NOT EXISTS <schema>`, run once before the migrators so the
/// unqualified `CREATE TABLE`s have a schema to land in when an operator points
/// the plugin at a non-`public` schema (PGR-L4). `schema` must have passed
/// [`validate_schema`]. A no-op for the default `public` schema.
///
/// # Errors
/// Propagates any connectivity/permission error creating the schema.
pub async fn ensure_schema(pool: &sqlx::PgPool, schema: &str) -> Result<(), ClusterError> {
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(pool)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

/// Releases every session-level advisory lock still held on a connection being
/// returned to the pool (PGR-L3 cancellation safety net).
///
/// A lock acquisition future can be cancelled after `pg_try_advisory_lock`
/// succeeds but before the connection is pinned in `PostgresLock`'s `held` map
/// (e.g. `lock()`'s per-attempt timeout elapsing mid-acquire): the pooled
/// connection would otherwise return to the pool still carrying a live advisory
/// lock, which the TTL reaper can never find (no `cluster_lock` row was
/// committed) and which no other session can release (advisory locks are
/// session-scoped) — wedging that lock name until the connection is recycled.
/// `pg_advisory_unlock_all()` runs on every checkin as a defensive sweep. It is
/// a no-op for connections that hold nothing (the common case, and every cache
/// connection), and for a legitimately held lock it is *also* a no-op here:
/// pinned lock connections are only returned to the pool after `release`/
/// `reclaim` already unlocked them. Returns `Ok(true)` so the connection is
/// kept; a failed sweep discards it rather than risk returning a locked
/// connection to the pool.
pub async fn release_session_advisory_locks(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(conn)
        .await?;
    Ok(true)
}

/// Rejects `pgbouncer_transaction_mode: true` at startup (DESIGN.md §5.4):
/// session-level advisory locks are released the moment `PgBouncer` returns the
/// connection to the pool between transactions in transaction-pooling mode,
/// even though the Rust code still holds a `LockGuard` — silent, hard to
/// diagnose mis-behaviour rather than a clear startup failure.
pub fn reject_pgbouncer_transaction_mode(enabled: bool) -> Result<(), ClusterError> {
    if enabled {
        return Err(ClusterError::InvalidConfig {
            reason: "pg_advisory_lock requires session-mode pooling or a direct connection; \
                      transaction-mode PgBouncer is incompatible with distributed locks"
                .to_owned(),
        });
    }
    Ok(())
}

/// `SET synchronous_commit = on`, enforced on every connection this plugin
/// uses (DESIGN.md §3.4) via both `after_connect` (new connections) and
/// `before_acquire` (re-asserted on every checkout, since the setting is
/// `USERSET` scope and can be mutated mid-session by anything sharing the
/// connection).
pub async fn set_synchronous_commit(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SET synchronous_commit = on")
        .execute(conn)
        .await?;
    Ok(())
}

/// The embedded `Migrator` over `migrations/cache/` (`0001_cluster_cache.sql`
/// only) — see `lib.rs`'s crate doc / DESIGN.md §3.1 for why this is a
/// separate `Migrator` from [`lock_migrator`], not one shared folder.
pub fn cache_migrator() -> Migrator {
    sqlx::migrate!("./src/migrations/cache")
}

/// The embedded `Migrator` over `migrations/lock/` (`0002_cluster_lock.sql`
/// only).
pub fn lock_migrator() -> Migrator {
    sqlx::migrate!("./src/migrations/lock")
}

/// Runs `migrator` against `pool`, tolerating the *other* Migrator's version
/// already being recorded in the database's shared `_sqlx_migrations` table
/// (DESIGN.md §3.1) — required whenever the combined plugin and the
/// standalone lock plugin ever point at the same database.
pub async fn run_migrator(mut migrator: Migrator, pool: &sqlx::PgPool) -> Result<(), ClusterError> {
    migrator.set_ignore_missing(true);
    migrator
        .run(pool)
        .await
        .map_err(|err| ClusterError::Provider {
            kind: cluster_sdk::ProviderErrorKind::Other,
            message: format!("migration failed: {err}"),
        })
}

/// Detects (or trusts an explicit override for) replication topology and logs
/// `cluster.provider.replication_async` once if the effective mode is
/// `Async` (DESIGN.md §3.6). Never fails startup on its own account — only a
/// genuine connectivity error querying `SHOW synchronous_standby_names`
/// propagates.
pub async fn warn_if_async_replication(
    pool: &sqlx::PgPool,
    configured: Option<ReplicationMode>,
) -> Result<(), ClusterError> {
    let effective = if let Some(mode) = configured {
        mode
    } else {
        let synchronous_standby_names: String =
            sqlx::query_scalar("SHOW synchronous_standby_names")
                .fetch_one(pool)
                .await
                .map_err(map_sqlx_error)?;
        if synchronous_standby_names.is_empty() {
            ReplicationMode::Async
        } else {
            ReplicationMode::Sync
        }
    };

    if effective == ReplicationMode::Async {
        warn!(
            "cluster.provider.replication_async: no synchronous standby configured - \
             per ADR-009's safety table, leader-election/lock claims are not failover-safe \
             under this replication topology"
        );
    }
    Ok(())
}
