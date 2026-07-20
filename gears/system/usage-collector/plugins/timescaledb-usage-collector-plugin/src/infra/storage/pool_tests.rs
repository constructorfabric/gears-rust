use super::*;
use sqlx::postgres::PgSslMode;

// `PgSslMode` derives no `PartialEq`, so assertions match the variant.

#[test]
fn connect_options_upgrades_prefer_to_require() {
    let opts = connect_options("postgres://u:p@h/db?sslmode=prefer").expect("valid dsn");
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::Require),
        "a weaker `prefer` mode must be upgraded to `require`"
    );
}

#[test]
fn connect_options_upgrades_allow_to_require() {
    let opts = connect_options("postgres://u:p@h/db?sslmode=allow").expect("valid dsn");
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::Require),
        "a silent `allow` fallback must be upgraded to `require`"
    );
}

#[test]
fn connect_options_honors_explicit_disable() {
    // An explicit `disable` is a deliberate, auditable opt-out (local / tests).
    let opts = connect_options("postgres://u:p@h/db?sslmode=disable").expect("valid dsn");
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::Disable),
        "an explicit `disable` is a deliberate opt-out and must be honored"
    );
}

#[test]
fn connect_options_defaults_unspecified_dsn_to_require() {
    // sqlx's default is `prefer` (plaintext fallback); enforcement makes it `require`.
    let opts = connect_options("postgres://u:p@h/db").expect("valid dsn");
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::Require),
        "a DSN without an explicit sslmode must default to `require`, not `prefer`"
    );
}

#[test]
fn connect_options_preserves_stronger_verify_full() {
    let opts = connect_options("postgres://u:p@h/db?sslmode=verify-full").expect("valid dsn");
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::VerifyFull),
        "an operator's stronger `verify-full` must not be downgraded to `require`"
    );
}

#[test]
fn connect_options_rejects_malformed_dsn() {
    assert!(connect_options("not a dsn").is_err());
}

#[test]
fn connection_gucs_bind_statement_and_fixed_lock_timeout() {
    // The statement timeout is config-driven (seconds -> `<n>s`); the lock timeout
    // is a fixed constant so a contended row lock fails fast rather than blocking.
    let gucs = connection_gucs(45);
    assert_eq!(gucs[0], ("statement_timeout", "45s".to_owned()));
    assert_eq!(gucs[1], ("lock_timeout", LOCK_TIMEOUT.to_owned()));
}

#[test]
fn pool_connect_options_sets_statement_and_lock_timeouts() {
    // The request-path connect options must carry both GUCs as `-c` startup
    // parameters so every pooled connection is bounded at connect time.
    let opts = pool_connect_options("postgres://u:p@h/db?sslmode=require", 45).expect("valid dsn");
    let applied = opts.get_options().expect("runtime options must be set");
    assert!(
        applied.contains("statement_timeout=45s"),
        "statement_timeout GUC missing; got: {applied}"
    );
    assert!(
        applied.contains("lock_timeout=5s"),
        "lock_timeout GUC missing; got: {applied}"
    );
    // TLS enforcement from `connect_options` still applies through the same builder.
    assert!(
        matches!(opts.get_ssl_mode(), PgSslMode::Require),
        "pool_connect_options must preserve TLS enforcement"
    );
}

#[test]
fn is_plaintext_only_true_for_disable() {
    // Only an explicit `disable` reaches here as plaintext: the silent fallbacks
    // (`prefer`/`allow`) are upgraded to `require` before this predicate runs.
    assert!(is_plaintext(PgSslMode::Disable));
    assert!(!is_plaintext(PgSslMode::Require));
    assert!(!is_plaintext(PgSslMode::VerifyCa));
    assert!(!is_plaintext(PgSslMode::VerifyFull));
}
