//! Shared pool/migration/startup-validation helpers used by both the
//! combined [`crate::PostgresClusterPlugin`] and the standalone
//! [`crate::PostgresLockPlugin`] builders (DESIGN.md §3.2, §3.5).

use cluster_sdk::ClusterError;
use sqlx::AssertSqlSafe;
use sqlx::PgConnection;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use tracing::warn;

use crate::config::ReplicationMode;
use crate::pg_error::map_sqlx_error;

/// A [`PgPoolOptions`] with the `after_connect`/`before_acquire` hooks wired
/// (DESIGN.md §3.4) — callers still need to chain `.max_connections(...)`,
/// `.acquire_timeout(...)`, and `.connect(...)`.
///
/// `after_connect` pins every connection's `search_path` to `schema` so the
/// migrations' unqualified `CREATE TABLE`s land in the same schema the runtime
/// queries target with their `{schema}.cluster_*` qualifiers (PGR-L4). `schema`
/// must already have passed [`validate_schema`] — it is interpolated unquoted.
///
/// **No `after_release` hook.** An earlier revision swept every checked-in
/// connection with `pg_advisory_unlock_all()`, because back then a lock
/// acquisition ran `pg_try_advisory_lock` on a *pooled* connection and a cancelled
/// acquisition could return one to the pool still holding a live lock. No lock
/// takes an advisory lock at all now — the `cluster_lock` row is the arbiter
/// (DESIGN.md §5.1) — so the sweep would be a pure per-checkin round-trip on the
/// cache hot path, doubling the pool's per-operation hook overhead to protect
/// against a state this crate can no longer produce. The one advisory lock this
/// plugin still takes is the liveness beacon, which lives on its own connection
/// outside the pool and must **never** be released while the process runs
/// (`lock::beacon`); an `unlock_all` on a pooled connection could not reach it,
/// and one that could would be catastrophic.
///
/// One caveat, stated precisely because it is easy to get wrong: `sqlx`'s own
/// `Migrator::run` **does** take an advisory lock on a pooled connection, and it is
/// the *blocking* single-`bigint` form. Reading `sqlx 0.8.6`
/// (`sqlx-core/src/migrate/migrator.rs`), `conn.lock()` is taken first, every step
/// after it is `?`-propagated, and `conn.unlock()` is reached **only on the success
/// path** — so a migration that fails partway (dirty version, checksum mismatch,
/// failing DDL) returns that connection to the pool still holding the migrator's
/// lock, and neither builder here calls `pool.close()` on that path (the pool is
/// merely dropped). Two things keep that from mattering today: the migrator's key
/// uses the one-argument form (`pg_locks.objsubid = 1`) which cannot collide with
/// this plugin's two-argument keys (`objsubid = 2`), and a failed
/// `build_and_start` propagates to a caller that discards the whole handle, so the
/// sockets do close. Anything that starts reusing a pool across a failed
/// `build_and_start` would need to revisit this.
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
    sqlx::query(AssertSqlSafe(format!("SET search_path TO {schema}")))
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
    sqlx::query(AssertSqlSafe(format!(
        "CREATE SCHEMA IF NOT EXISTS {schema}"
    )))
    .execute(pool)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

/// Rejects `pgbouncer_transaction_mode: true` at startup (DESIGN.md §5.4).
///
/// Narrower than it once was, but not gone. Lock *operations* are now single
/// statements against the pool and would tolerate transaction-mode pooling
/// perfectly well. What cannot is the pair of connections this plugin opens
/// outside the pool: the liveness beacon, whose session-scoped advisory lock
/// transaction-mode pooling would release between transactions — asserting to
/// the whole fleet that this instance is dead while it still holds live locks —
/// and the `LISTEN` connections, whose subscriptions have the same session
/// affinity.
///
/// Both are opened directly rather than through the pool, so in practice this
/// guards an operator who has put `PgBouncer` in front of the DSN itself. Silent,
/// hard-to-diagnose misbehaviour rather than a clear startup failure is exactly
/// what it is worth refusing to start over.
pub fn reject_pgbouncer_transaction_mode(enabled: bool) -> Result<(), ClusterError> {
    if enabled {
        return Err(ClusterError::InvalidConfig {
            reason: "the liveness beacon's advisory lock and the LISTEN subscriptions require \
                      session-mode pooling or a direct connection; transaction-mode PgBouncer \
                      would release the beacon between transactions, telling the fleet this \
                      instance is dead while it still holds locks"
                .to_owned(),
        });
    }
    Ok(())
}

/// The transaction isolation level this plugin's guarded upserts require, as
/// `SHOW transaction_isolation` reports it.
const REQUIRED_ISOLATION: &str = "read committed";

/// Asserts the server hands this plugin `READ COMMITTED` transactions, failing
/// startup with [`ClusterError::InvalidConfig`] otherwise.
///
/// Both primitives arbitrate a claim with the same idiom — an
/// `INSERT ... ON CONFLICT DO UPDATE ... WHERE <may I take it?>` whose losing
/// side must observe the winner's *already-committed* row. Postgres re-reads the
/// latest committed version of a conflicting tuple only under `READ COMMITTED`;
/// under `REPEATABLE READ` or `SERIALIZABLE` the transaction snapshot cannot
/// advance, so instead of re-evaluating the predicate the statement raises
/// SQLSTATE `40001` (`could not serialize access due to concurrent update`) and
/// the caller is expected to retry. Neither `cache::put_if_absent` (the
/// leader-election failover path, via `CasBasedLeaderElectionBackend::claim`)
/// nor the lock's acquire does that, so a misconfigured server turns ordinary
/// contention into a `Provider` error.
///
/// Asserted, not enforced. One
/// `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED`
/// in [`base_pool_options`]'s `after_connect` — beside the `synchronous_commit`
/// enforcement — would make the property true regardless of the server default,
/// and is available cheaply if wanted. It is deliberately not done: silently
/// overriding an isolation level an operator set on purpose hides a mismatch
/// that failing fast surfaces, and the failure this guards against is a loud,
/// distinctively-coded error rather than silent misbehaviour. Same precedent as
/// [`reject_pgbouncer_transaction_mode`] (DESIGN.md §5.4).
///
/// # Errors
/// [`ClusterError::InvalidConfig`] if the effective isolation level is anything
/// other than `read committed`; a genuine connectivity error propagates as
/// itself.
pub async fn assert_read_committed(pool: &sqlx::PgPool) -> Result<(), ClusterError> {
    let isolation: String = sqlx::query_scalar("SHOW transaction_isolation")
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_error)?;
    if !isolation.eq_ignore_ascii_case(REQUIRED_ISOLATION) {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "transaction_isolation is {isolation:?}, but this plugin requires \
                 {REQUIRED_ISOLATION:?}: both the lock's acquire and the cache's put_if_absent \
                 are guarded upserts whose losing side must re-read the winner's committed row, \
                 which stricter isolation levels answer with a serialization failure instead. Set \
                 the server's default_transaction_isolation (or the DSN's options) to read \
                 committed"
            ),
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
