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
/// | SQLSTATE `54000` / `23514` (over-long indexed key) | `Provider { Other }`, message rewritten to name the length limit |
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
    // An over-long indexed key is normally caught in Rust before the write
    // (`cache::watch::validate_key_len`, `lock::validate_lock_name`, both bounded
    // by `limits::MAX_INDEXED_KEY_BYTES`). If one still reaches Postgres, the
    // server's own message is opaque about the cause — `54000` reads as `index
    // row size 2712 exceeds btree version 4 maximum 2704 for index "..."` and
    // `23514` names only the constraint. Rewrite both to say what actually has to
    // change, since neither is retryable and the raw text does not point at the
    // key. `Other` is the correct kind for both: not retryable, operator/caller
    // action required (DESIGN.md §2.1 / §9).
    if let sqlx::Error::Database(db_err) = &err
        && let Some(message) = over_long_key_message(db_err.as_ref())
    {
        return ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message,
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

/// Returns an explanatory message when `db_err` is Postgres rejecting an
/// over-long value for one of this plugin's indexed primary keys, or `None` for
/// any other database error.
///
/// Two SQLSTATEs mean this, depending on which guard the value tripped first:
/// `54000` (`program_limit_exceeded`) is the btree refusing an index tuple past
/// its ~2704-byte ceiling, and `23514` (`check_violation`) is one of the
/// `*_len_check` CHECK constraints the migrations declare in front of it. The
/// `23514` arm matches on the constraint name so an unrelated future CHECK is
/// not mislabelled as a length problem.
fn over_long_key_message(db_err: &dyn sqlx::error::DatabaseError) -> Option<String> {
    const LEN_CHECK_CONSTRAINTS: [&str; 2] =
        ["cluster_cache_key_len_check", "cluster_lock_name_len_check"];

    let is_over_long = match db_err.code().as_deref()? {
        "54000" => true,
        "23514" => db_err
            .constraint()
            .is_some_and(|name| LEN_CHECK_CONSTRAINTS.contains(&name)),
        _ => false,
    };

    is_over_long.then(|| {
        format!(
            "key or lock name exceeds the {}-byte maximum this plugin indexes \
             (the btree index-tuple limit on the cluster_cache / cluster_lock primary key; \
             DESIGN.md sec 2.1): {db_err}",
            crate::limits::MAX_INDEXED_KEY_BYTES,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::map_sqlx_error;
    use cluster_sdk::{ClusterError, ProviderErrorKind};
    use sqlx::error::{DatabaseError, ErrorKind};

    /// A stand-in for `PgDatabaseError`, whose fields the driver keeps private —
    /// only the three accessors `over_long_key_message` reads need to be real.
    #[derive(Debug)]
    struct FakeDbError {
        code: &'static str,
        constraint: Option<&'static str>,
    }

    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("server said no")
        }
    }

    impl std::error::Error for FakeDbError {}

    impl DatabaseError for FakeDbError {
        fn message(&self) -> &'static str {
            "server said no"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.code))
        }
        fn constraint(&self) -> Option<&str> {
            self.constraint
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
    }

    fn map(code: &'static str, constraint: Option<&'static str>) -> ClusterError {
        map_sqlx_error(sqlx::Error::Database(Box::new(FakeDbError {
            code,
            constraint,
        })))
    }

    #[test]
    fn btree_index_tuple_overflow_is_explained() {
        // 54000 carries no constraint name — the btree rejects the tuple before
        // any CHECK runs — so the code alone has to be enough to recognize it.
        let err = map("54000", None);
        let ClusterError::Provider { kind, message } = err else {
            panic!("expected a Provider error, got {err:?}");
        };
        assert_eq!(kind, ProviderErrorKind::Other);
        assert!(
            message.contains("2048-byte maximum"),
            "the message must name the limit the caller has to get under, got {message:?}"
        );
        assert!(
            message.contains("server said no"),
            "the underlying server message must be preserved, got {message:?}"
        );
    }

    #[test]
    fn length_check_violations_are_explained_for_both_tables() {
        for constraint in ["cluster_cache_key_len_check", "cluster_lock_name_len_check"] {
            let err = map("23514", Some(constraint));
            let ClusterError::Provider { message, .. } = err else {
                panic!("expected a Provider error for {constraint}, got {err:?}");
            };
            assert!(
                message.contains("2048-byte maximum"),
                "{constraint} must map to the length explanation, got {message:?}"
            );
        }
    }

    #[test]
    fn an_unrelated_check_violation_is_not_mislabelled() {
        // The point of matching on the constraint name: a future CHECK that has
        // nothing to do with key length must fall through to the generic arm
        // rather than telling the caller to shorten a key.
        let err = map("23514", Some("cluster_lock_ttl_positive_check"));
        let ClusterError::Provider { kind, message } = err else {
            panic!("expected a Provider error, got {err:?}");
        };
        assert_eq!(kind, ProviderErrorKind::Other);
        assert!(
            !message.contains("2048-byte maximum"),
            "an unrelated CHECK must not be reported as a length problem, got {message:?}"
        );
    }

    #[test]
    fn auth_failure_mapping_still_wins_for_28xxx() {
        // The new arm runs before the existing SQLSTATE match; make sure it did
        // not shadow a code it should ignore.
        let err = map("28P01", None);
        let ClusterError::Provider { kind, .. } = err else {
            panic!("expected a Provider error, got {err:?}");
        };
        assert_eq!(kind, ProviderErrorKind::AuthFailure);
    }
}
