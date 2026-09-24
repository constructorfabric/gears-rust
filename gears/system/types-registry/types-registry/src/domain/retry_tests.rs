use super::{database_failure_may_clear, scoped_failure_may_clear};
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

#[test]
fn engine_and_transport_failures_are_retried() {
    assert!(database_failure_may_clear(&DbError::Sea(
        sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout)
    )));
    assert!(database_failure_may_clear(&DbError::Io(
        std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset",)
    )));
}

#[test]
fn an_advisory_lock_failure_is_retried_like_the_errors_it_wraps() {
    let io = toolkit_db::advisory_locks::DbLockError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    ));

    assert!(
        database_failure_may_clear(&DbError::Lock(io)),
        "a lock failure carrying a transport error must not be permanent",
    );
}

#[test]
fn configuration_and_programming_failures_are_permanent() {
    assert!(!database_failure_may_clear(&DbError::InvalidConfig(
        "invalid configuration".into()
    )));
    assert!(!database_failure_may_clear(&DbError::UnknownDsn(
        "no-such-dsn".into()
    )));
    assert!(!database_failure_may_clear(&DbError::FeatureDisabled("pg")));
    assert!(!database_failure_may_clear(&DbError::ConnRequestedInsideTx));
}

#[test]
fn scope_decisions_are_permanent_but_a_scoped_database_failure_is_not() {
    assert!(!scoped_failure_may_clear(&ScopeError::Denied(
        "not allowed"
    )));
    assert!(!scoped_failure_may_clear(&ScopeError::Invalid(
        "invalid scope"
    )));
    assert!(!scoped_failure_may_clear(&ScopeError::GraphSyntax(
        "no projected columns".into()
    )));
    assert!(!scoped_failure_may_clear(&ScopeError::TenantNotInScope {
        tenant_id: uuid::Uuid::nil(),
    }));

    assert!(scoped_failure_may_clear(&ScopeError::Db(
        sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout)
    )));
}

#[test]
fn a_scope_denial_wrapped_by_the_provider_stays_permanent() {
    let wrapped = DbError::Other(anyhow::Error::new(ScopeError::Denied("not allowed")));

    assert!(!database_failure_may_clear(&wrapped));
}

#[test]
fn an_opaque_wrapped_failure_takes_the_retry_side() {
    assert!(database_failure_may_clear(&DbError::Other(
        anyhow::anyhow!("connection reset")
    )));
}

#[test]
fn a_query_failure_is_retried_and_left_to_the_delivery_budget() {
    let missing_table = ScopeError::Db(sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(
        "no such table: operation".into(),
    )));

    assert!(
        scoped_failure_may_clear(&missing_table),
        "a query failure is bounded by worker.max_delivery_attempts, not by this classifier",
    );
}

#[test]
fn a_contended_lock_is_retried_but_a_misconfigured_one_is_not() {
    use toolkit_db::advisory_locks::DbLockError;

    assert!(
        database_failure_may_clear(&DbError::Lock(DbLockError::AlreadyHeld {
            lock_name: "types_registry__entity_write_order".to_owned(),
        })),
        "another holder releases; that is the whole premise of an advisory lock",
    );
    assert!(
        database_failure_may_clear(&DbError::Lock(DbLockError::UnexpectedDatabaseResult {
            message: "expected one row".to_owned(),
        })),
        "an unexpected result is the engine answering oddly, not a decided refusal",
    );

    assert!(
        !database_failure_may_clear(&DbError::Lock(DbLockError::InvalidConfig {
            message: "keepalive must be positive".to_owned(),
        })),
        "a redelivery reads the same lock configuration and is refused the same way",
    );
    assert!(
        !database_failure_may_clear(&DbError::Lock(DbLockError::NotHeld)),
        "releasing a lock this process never took is a bug here, not a transient state",
    );
}
