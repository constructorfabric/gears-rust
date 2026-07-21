//! Shared `sqlx::Error` → `ClusterError` mapping (DESIGN.md §9), used by both
//! the cache and lock backends.

use cluster_sdk::{ClusterError, ProviderErrorKind};

/// Maps a `sqlx::Error` to `ClusterError::Provider` per the `ProviderErrorKind`
/// mapping table (DESIGN.md §9):
///
/// | `sqlx` error | `ClusterError` |
/// |---|---|
/// | `sqlx::Error::Configuration` | `InvalidConfig` |
/// | `sqlx::Error::Io` | `Provider { ConnectionLost }` |
/// | `sqlx::Error::PoolTimedOut` | `Provider { Timeout }` |
/// | `sqlx::Error::PoolClosed` | `Provider { ConnectionLost }` |
/// | SQLSTATE `28xxx` (invalid auth) | `Provider { AuthFailure }` |
/// | Any other `sqlx::Error` | `Provider { Other }` |
///
/// Takes `err` by value (not `&sqlx::Error`) so call sites can pass this
/// directly as `.map_err(map_sqlx_error)` without a wrapping closure — the
/// dominant call pattern across `cache/mod.rs` — rather than for its own sake.
#[allow(clippy::needless_pass_by_value)]
pub fn map_sqlx_error(err: sqlx::Error) -> ClusterError {
    // A malformed connection string (bad DSN, unparseable options) surfaces as
    // `sqlx::Error::Configuration`. That is an operator *config* error, not a
    // runtime backend fault, so it maps to `InvalidConfig` rather than the
    // catch-all `Provider { Other }` (DESIGN.md §3.2 / §9, TESTING.md §2 /
    // `PG-LIFE-006`).
    if matches!(err, sqlx::Error::Configuration(_)) {
        return ClusterError::InvalidConfig {
            reason: err.to_string(),
        };
    }
    let kind = match &err {
        sqlx::Error::Io(_) | sqlx::Error::PoolClosed => ProviderErrorKind::ConnectionLost,
        sqlx::Error::PoolTimedOut => ProviderErrorKind::Timeout,
        sqlx::Error::Database(db_err) => {
            if db_err
                .code()
                .as_deref()
                .is_some_and(|code| code.starts_with("28"))
            {
                ProviderErrorKind::AuthFailure
            } else {
                ProviderErrorKind::Other
            }
        }
        _ => ProviderErrorKind::Other,
    };
    ClusterError::Provider {
        kind,
        message: err.to_string(),
    }
}
