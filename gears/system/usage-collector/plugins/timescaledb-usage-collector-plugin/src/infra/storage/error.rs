// Vendored TimescaleDB raw-SQL backend: `sqlx` is required infra (hypertable
// time-series, `time_bucket` aggregation, keyset pagination — see DESIGN.md). Tenant
// isolation is enforced by hand via parameterized `tenant_id` predicates and an
// allowlisted-identifier query builder (DESIGN.md §Injection-Safe Query Translation),
// not SecureConn/AccessScope.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

use usage_collector_sdk::UsageCollectorPluginError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbErrorClass {
    DedupUniqueViolation,
    CatalogUniqueViolation,
    ForeignKeyViolation,
    Transient,
    Other,
}

fn is_transient_sqlstate(code: &str) -> bool {
    code.starts_with("08")
        || matches!(
            code,
            "57P01" | "57P02" | "57P03" | "53300" | "40001" | "40P01"
        )
}

#[must_use]
pub fn classify_db(code: &str, constraint: Option<&str>) -> DbErrorClass {
    match code {
        // Match each unique constraint by name. A new unique constraint (or a
        // records PK `(id, created_at)` collision) must fall through to
        // `Other` rather than be silently misread as a catalog conflict.
        //
        // `usage_records_dedup_uniq` is the dedup authority, but the ingest path
        // reaches it via `INSERT … ON CONFLICT … DO NOTHING`, which suppresses
        // the 23505 — so this arm is defensive: it only fires if a dedup-unique
        // violation ever surfaces as a raw error (e.g. a future write path that
        // bypasses `ON CONFLICT`), keeping it classified rather than `Other`.
        "23505" => match constraint {
            Some("usage_records_dedup_uniq") => DbErrorClass::DedupUniqueViolation,
            Some("usage_type_catalog_pkey") => DbErrorClass::CatalogUniqueViolation,
            _ => DbErrorClass::Other,
        },
        "23503" => DbErrorClass::ForeignKeyViolation,
        c if is_transient_sqlstate(c) => DbErrorClass::Transient,
        _ => DbErrorClass::Other,
    }
}

/// Whether a `pool.acquire()` failure should clear the `uc_timescaledb_ready`
/// gauge — i.e. whether it indicates lost backend *connectivity* rather than a
/// healthy-but-saturated pool. `live_connections` is the pool's current
/// established-connection count (`PgPool::size`) at the time of the failure.
///
/// `PoolTimedOut` is the crux: sqlx funnels *both* pool saturation (every
/// connection checked out, backend healthy) *and* connection-establishment
/// failure (backend unreachable) into this one variant after the acquire
/// timeout elapses, so the variant alone cannot tell them apart. Occupancy
/// resolves it: a healthy-but-saturated pool always still holds its established
/// connections (`live_connections > 0` — "all busy", not an outage), whereas an
/// unreachable backend cannot keep any (`live_connections == 0` — a real
/// outage). This stops the gauge flapping under load (the false-positive)
/// without losing connection-refused outage detection (the most
/// common manifestation, which arrives as `PoolTimedOut` with zero live
/// connections). Sustained saturation is covered separately by the
/// `pool.connections.active` vs `pool_size_max` SLO.
#[must_use]
pub fn acquire_error_clears_readiness(err: &sqlx::Error, live_connections: u32) -> bool {
    match err {
        // Ambiguous timeout: saturation iff the pool still holds connections.
        sqlx::Error::PoolTimedOut => live_connections == 0,
        // A fresh physical connection was refused/reset, or its TLS handshake
        // failed, or the pool has been torn down: genuine loss of connectivity.
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed => true,
        // Backend-reported connection-class SQLSTATE (server shutdown, too many
        // connections, ...): treat exactly like the transient classification.
        sqlx::Error::Database(db) => is_transient_sqlstate(db.code().as_deref().unwrap_or("")),
        // Anything else (decode/protocol/config/query-shaped) is not an outage.
        _ => false,
    }
}

/// `(sqlstate, constraint)` if `err` is a DB error.
#[must_use]
pub fn db_code_and_constraint(err: &sqlx::Error) -> Option<(String, Option<String>)> {
    if let sqlx::Error::Database(db) = err {
        return Some((
            db.code()?.into_owned(),
            db.constraint().map(ToOwned::to_owned),
        ));
    }
    None
}

/// Catch-all mapping for non-classified sqlx errors (transient vs internal).
#[must_use]
pub fn map_sqlx_err(err: &sqlx::Error) -> UsageCollectorPluginError {
    if let sqlx::Error::Database(db) = err
        && classify_db(db.code().as_deref().unwrap_or(""), db.constraint())
            == DbErrorClass::Transient
    {
        return UsageCollectorPluginError::transient("transient database error");
    }
    // Connectivity-class transport faults are retryable. `Tls` is included to
    // stay consistent with `acquire_error_clears_readiness`, which already
    // treats a TLS failure as a connectivity outage — a query-time TLS blip
    // must lift to a retryable Transient, not a non-retryable Internal.
    if matches!(
        err,
        sqlx::Error::PoolTimedOut
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolClosed
    ) {
        return UsageCollectorPluginError::transient("database unavailable");
    }
    // Do NOT format the raw error into the user-facing detail: the SDK contract
    // (`UsageCollectorPluginError::Internal`) requires it to be DSN-free /
    // pre-redacted, and a `sqlx::Error` Display (e.g. `Configuration` / `Tls`
    // source chains) can carry connection-string fragments. Log the full error
    // for operators (logs/traces are the right home for diagnostics) and return
    // a fixed token.
    tracing::error!(error = %err, "unclassified backend error mapped to Internal");
    UsageCollectorPluginError::internal("database error")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
