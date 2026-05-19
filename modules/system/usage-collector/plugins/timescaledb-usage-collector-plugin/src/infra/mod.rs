// This storage plugin owns its database schema, hypertable, continuous aggregate
// and retention policy. Those are TimescaleDB-specific DDLs and policy calls
// that have no SecureORM/SecureConn equivalent — the framework lint that bans
// raw sqlx in ModKit modules is therefore deliberately disabled inside infra/.
// Access control is enforced at the query-builder layer (see `domain/scope.rs`,
// which produces a non-empty WHERE fragment from the PDP-compiled `AccessScope`
// and rejects empty scopes/InGroup predicates) — every read/write in this crate
// flows through that scope fragment.
//
// MODKIT-DEVIATION: de0706_no_direct_sqlx (a.k.a. MODKIT-DB-002 / MODKIT-SEC-001).
// The allow is module-wide because every file under `infra/` legitimately
// needs direct sqlx access (DDL helpers, dynamic aggregation/pagination SQL,
// retention policy, plain-table idempotency cleanup). The substitute boundary
// is `domain::scope::scope_to_sql`, which fails closed on empty scopes and
// rejects `InGroup`/`InGroupSubtree` predicates. See the
// "Architectural deviation" section in `../../README.md` and ADR-0003
// (`../../../../docs/ADR/0003-cpt-cf-usage-collector-adr-timescaledb-plugin-raw-sqlx.md`)
// for the full rationale. Maintainers adding new files under `infra/` MUST
// consult those docs before relying on this allow; in particular, no new file
// under `infra/` may bypass the `scope_to_sql` boundary for tenant-scoped reads
// or writes.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

pub mod continuous_aggregate;
pub mod db_error;
pub mod migrations;
pub mod otel_metrics;
pub mod pg_insert_port;
pub mod pg_query_port;
pub mod retention;

/// Returns `true` when `e` represents a transient `PostgreSQL` failure that the
/// caller should treat as retryable.
///
/// Transient SQLSTATE codes covered:
/// - `40001` `serialization_failure` / `40P01` `deadlock_detected`
/// - `57P03` `cannot_connect_now` / `53300` `too_many_connections`
/// - `08006` `connection_failure` / `08001` `sqlclient_unable_to_establish_sqlconnection`
///
/// `sqlx::Error::PoolTimedOut`, `PoolClosed`, and `Io(_)` are also classified as
/// transient. Single source of truth shared by `pg_insert_port` and
/// `pg_query_port` so the two paths can't drift.
#[must_use]
pub fn is_transient_pg_error(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => true,
        sqlx::Error::Database(db_err) => matches!(
            db_err.code().as_deref(),
            Some("40001" | "40P01" | "57P03" | "53300" | "08006" | "08001")
        ),
        _ => false,
    }
}
