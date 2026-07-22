// Vendored TimescaleDB raw-SQL backend: `sqlx` is required infra (hypertable
// time-series, `time_bucket` aggregation, keyset pagination — see DESIGN.md). Tenant
// isolation is enforced by hand via parameterized `tenant_id` predicates and an
// allowlisted-identifier query builder (DESIGN.md §Injection-Safe Query Translation),
// not SecureConn/AccessScope.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

use std::str::FromStr;
use std::time::Duration;

use secrecy::ExposeSecret;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{PgConnection, PgPool};

use crate::config::TimescaleDbPluginConfig;

/// Embedded schema migrations (`migrations/` at crate root).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Parse the DSN into connection options with TLS enforced by default.
///
/// TLS is the plugin's one stated security obligation (DESIGN §3.5 / §Security),
/// so it is enforced here rather than left to operator DSN convention: sqlx's
/// default is `prefer`, which silently falls back to plaintext. The silent
/// modes — an unspecified `sslmode`, `prefer`, or `allow` — are upgraded to
/// `require` so credentials and usage data are never sent in cleartext by
/// omission. A stronger operator choice (`verify-ca` / `verify-full`) is
/// preserved.
///
/// An explicit `sslmode=disable` is honored as a deliberate, auditable opt-out
/// (trusted networks / local dev / the integration test container, which serves
/// no TLS). This still closes the defect — the *silent* plaintext fallback —
/// while leaving a documented escape hatch.
///
/// # Errors
/// Returns `sqlx::Error` if the DSN cannot be parsed.
fn connect_options(database_url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    let opts = PgConnectOptions::from_str(database_url)?;
    let resolved = match opts.get_ssl_mode() {
        // Silent fallback modes (incl. the unspecified default `prefer`): upgrade.
        PgSslMode::Allow | PgSslMode::Prefer => opts.ssl_mode(PgSslMode::Require),
        // Explicit `disable` is a deliberate opt-out; `require` and the verifying
        // modes already meet the obligation. All kept as-is.
        PgSslMode::Disable | PgSslMode::Require | PgSslMode::VerifyCa | PgSslMode::VerifyFull => {
            opts
        }
    };
    if is_plaintext(resolved.get_ssl_mode()) {
        // A documented-but-silent plaintext opt-out is invisible to operators, so
        // surface it in logs/alerting at startup. Emitted once per pool build
        // (this fn is called once from `build_pool`), not once per connection.
        tracing::warn!(
            "connecting to TimescaleDB with sslmode=disable: credentials and usage \
             data are sent in cleartext. This is a deliberate opt-out; set \
             sslmode=require (or verify-ca/verify-full) for encrypted transport."
        );
    }
    Ok(resolved)
}

/// Whether the enforced SSL mode is a plaintext opt-out (data sent in cleartext).
/// Only an explicit `disable` reaches here as plaintext, since [`connect_options`]
/// upgrades the silent fallbacks (`prefer`/`allow`) to `require` first.
fn is_plaintext(mode: PgSslMode) -> bool {
    matches!(mode, PgSslMode::Disable)
}

/// Fixed upper bound on how long a request-path statement waits to acquire a row
/// lock (e.g. the deactivate `SELECT ... FOR UPDATE`). A contended lock then fails
/// fast (`55P03 lock_not_available`) instead of blocking on — and pinning — a
/// pooled connection.
const LOCK_TIMEOUT: &str = "5s";

/// Session GUCs applied to every request-path pool connection at connect time,
/// bounding how long a statement may run (`statement_timeout`, config-driven) and
/// how long it waits on a row lock (`lock_timeout`, fixed [`LOCK_TIMEOUT`]) so a
/// wedged backend cannot pin pool connections indefinitely and exhaust the pool.
/// Applied as `-c name=value` startup parameters so the bound holds from the
/// connection's first query, with no extra round-trip.
fn connection_gucs(statement_timeout_secs: u64) -> [(&'static str, String); 2] {
    [
        ("statement_timeout", format!("{statement_timeout_secs}s")),
        ("lock_timeout", LOCK_TIMEOUT.to_owned()),
    ]
}

/// Build the request-path connect options: DSN parsing + TLS enforcement
/// ([`connect_options`]) plus the bounding session GUCs ([`connection_gucs`]).
///
/// # Errors
/// Returns `sqlx::Error` if the DSN cannot be parsed.
fn pool_connect_options(
    database_url: &str,
    statement_timeout_secs: u64,
) -> Result<PgConnectOptions, sqlx::Error> {
    Ok(connect_options(database_url)?.options(connection_gucs(statement_timeout_secs)))
}

/// Build the connection pool with TLS enforced (`sslmode >= require`, see
/// [`connect_options`]) and every request-path connection bounded by
/// `statement_timeout` + `lock_timeout` (see [`connection_gucs`]).
///
/// # Errors
/// Returns `sqlx::Error` if the DSN is malformed or the pool cannot connect
/// within the timeout.
pub async fn build_pool(cfg: &TimescaleDbPluginConfig) -> Result<PgPool, sqlx::Error> {
    // Unwrap the secret DSN only here, at the connection boundary: keep it behind
    // `secrecy`'s opaque-debug/zeroize guarantees and expose the bytes just long
    // enough for sqlx to parse them into `PgConnectOptions`.
    let dsn = cfg.database_url.clone_into_secret_string();
    PgPoolOptions::new()
        .min_connections(cfg.pool_size_min)
        .max_connections(cfg.pool_size_max)
        .acquire_timeout(Duration::from_secs(cfg.connection_timeout_secs))
        .connect_with(pool_connect_options(
            dsn.expose_secret(),
            cfg.statement_timeout_secs,
        )?)
        .await
}

/// Fixed advisory-lock key namespacing the plugin's post-migration setup.
/// Arbitrary but stable; the plugin owns its database, so a collision with an
/// unrelated advisory lock is not a concern. (`0x7563_7462` == ASCII `"uctb"`.)
const INIT_ADVISORY_LOCK_KEY: i64 = 0x7563_7462;

/// Acquire the init advisory lock on `lock_conn`, serializing concurrent replica
/// init so only one applies the post-migration policy registration at a time.
///
/// `pg_advisory_lock` has no timeout of its own, but `lock_conn` is drawn from
/// [`build_pool`], which sets `statement_timeout` on every connection (see
/// [`connection_gucs`]). That connection-level bound aborts the blocking
/// `SELECT pg_advisory_lock(...)` with `57014` (query-canceled) if a wedged peer
/// holds the lock, so init fails fast and the orchestrator can retry instead of
/// stalling indefinitely. No per-lock `statement_timeout` override is applied, so
/// nothing is left set on the connection when it returns to the pool.
///
/// # Errors
/// Returns `sqlx::Error` if the lock cannot be acquired (including a
/// `statement_timeout` abort while a peer holds it).
async fn acquire_init_lock(lock_conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    if let Err(e) = sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(INIT_ADVISORY_LOCK_KEY)
        .execute(&mut *lock_conn)
        .await
    {
        tracing::error!(
            error = %e,
            "failed to acquire init advisory lock; a peer replica may be holding it, \
             so init fails fast to be retried"
        );
        return Err(e);
    }
    Ok(())
}

/// Run the post-migration policy registration under a database advisory lock so
/// concurrently-initializing replicas serialize here.
///
/// [`apply_retention_policy`]'s remove-then-add sequence is non-atomic, and
/// `add_retention_policy` (no `if_not_exists`) *errors* if a policy already
/// exists — so two pods racing this section could leave a half-applied state or
/// fail outright. A session-level `pg_advisory_lock` held on a dedicated
/// connection for the whole section lets only one replica apply at a time; the
/// rest block until it releases. (Schema migrations themselves are already
/// serialized by sqlx's own migration lock; this covers the registration that
/// sqlx does not.)
///
/// The policy/job functions keep running in autocommit on the pool, exactly as
/// before — deliberately *not* wrapped in an explicit transaction, since
/// `TimescaleDB` policy functions are happiest in autocommit. The lock is
/// released on every return path; if the holding process dies, Postgres releases
/// it when the session ends.
///
/// The wait to *acquire* the lock is bounded by the connection-level
/// `statement_timeout` (see [`acquire_init_lock`]) so a wedged peer cannot stall
/// init forever.
///
/// # Errors
/// Returns `sqlx::Error` if the lock cannot be acquired or either registration
/// step fails.
pub async fn apply_post_migration_setup(
    pool: &PgPool,
    retention_secs: u64,
) -> Result<(), sqlx::Error> {
    // Hold a session-level advisory lock on a dedicated connection for the whole
    // critical section. Concurrent replicas block on this `pg_advisory_lock`
    // until the holder releases it below, so only one applies at a time. The wait
    // is bounded by the connection-level `statement_timeout` (see
    // `acquire_init_lock`) so a wedged peer cannot stall init forever.
    let mut lock_conn = pool.acquire().await?;
    acquire_init_lock(&mut lock_conn).await?;

    let result = apply_retention_policy(pool, retention_secs).await;

    // Release on every path (including the error path) so a failing replica
    // never wedges the others. If the unlock itself fails the session is likely
    // already broken, in which case Postgres frees the lock when it ends.
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(INIT_ADVISORY_LOCK_KEY)
        .execute(&mut *lock_conn)
        .await
    {
        tracing::warn!(
            error = %e,
            "failed to release init advisory lock; it frees when the session ends"
        );
    }

    result
}

/// Idempotently register the config-driven retention policy, **updating** it if
/// it already exists so a changed `retention_secs` takes effect on restart.
/// Runs after migrations.
///
/// `add_retention_policy(if_not_exists => TRUE)` would *skip* an existing
/// policy and silently keep the old window; remove-then-add applies the new
/// one. The sub-second gap with no policy is harmless — retention is a slow
/// background job.
///
/// # Errors
/// Returns `sqlx::Error` if either statement fails.
pub async fn apply_retention_policy(pool: &PgPool, retention_secs: u64) -> Result<(), sqlx::Error> {
    let secs = i64::try_from(retention_secs).unwrap_or(i64::MAX);
    sqlx::query("SELECT remove_retention_policy('usage_records', if_exists => TRUE)")
        .execute(pool)
        .await?;
    sqlx::query(
        "SELECT add_retention_policy('usage_records', \
         drop_after => make_interval(secs => $1::double precision))",
    )
    .bind(secs)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "pool_tests.rs"]
mod pool_tests;
