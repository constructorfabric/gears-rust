#![allow(clippy::expect_used, clippy::unwrap_used)]

use time::Duration;
use toolkit_db::Db;
use toolkit_security::AccessScope;

use super::*;
use crate::infra::storage::repo::exception_repo::OrmExceptionRepo;
use crate::test_support::{TestClock, migrated_db};

fn key(tenant: Uuid, reason: &str) -> ExceptionKey {
    ExceptionKey::new(tenant, SubjectKind::Child, "child-1", reason).unwrap()
}

fn register(clock: Arc<TestClock>) -> ExceptionRegister {
    ExceptionRegister::new(Arc::new(OrmExceptionRepo), clock)
}

async fn record(
    db: &Db,
    register: &ExceptionRegister,
    scope: &AccessScope,
    key: &ExceptionKey,
    detail: &str,
) -> Result<ExceptionRecord, DomainError> {
    let (register, scope, key, detail) = (
        register.clone(),
        scope.clone(),
        key.clone(),
        detail.to_owned(),
    );
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move { register.record(tx, &scope, &key, &detail).await })
    })
    .await
}

async fn resolve(
    db: &Db,
    register: &ExceptionRegister,
    scope: &AccessScope,
    key: &ExceptionKey,
) -> Result<bool, DomainError> {
    let (register, scope, key) = (register.clone(), scope.clone(), key.clone());
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move { register.resolve(tx, &scope, &key, "succeeded").await })
    })
    .await
}

#[test]
fn every_kind_round_trips_its_name() {
    for kind in SubjectKind::ALL {
        assert_eq!(SubjectKind::parse(kind.as_str()), Some(kind));
    }
}

#[test]
fn keys_are_bounded() {
    let tenant = Uuid::from_u128(1);
    assert!(ExceptionKey::new(tenant, SubjectKind::Fact, "", "r").is_err());
    assert!(ExceptionKey::new(tenant, SubjectKind::Fact, "s", "").is_err());
    assert!(ExceptionKey::new(tenant, SubjectKind::Fact, "s", "r".repeat(256)).is_err());
}

#[tokio::test]
async fn one_open_row_per_subject_and_reason_counts_its_attempts() {
    let db = migrated_db().await;
    let clock = TestClock::new();
    let register = register(Arc::clone(&clock));
    let tenant = Uuid::from_u128(1);
    let scope = AccessScope::for_tenant(tenant);

    let first = record(
        &db,
        &register,
        &scope,
        &key(tenant, "awaiting_attribution"),
        "first",
    )
    .await
    .unwrap();
    clock.advance(Duration::minutes(5));
    let second = record(
        &db,
        &register,
        &scope,
        &key(tenant, "awaiting_attribution"),
        "second",
    )
    .await
    .unwrap();
    assert_eq!(second.exception_id, first.exception_id);
    assert_eq!(second.attempts, 2);
    assert_eq!(second.detail, "second");
    assert_eq!(second.first_seen_at, first.first_seen_at);
    assert!(second.last_attempt_at > first.last_attempt_at);

    // Another reason for the same subject is its own exception.
    let other = record(
        &db,
        &register,
        &scope,
        &key(tenant, "invalidation_target_unknown"),
        "x",
    )
    .await
    .unwrap();
    assert_ne!(other.exception_id, first.exception_id);
    assert_eq!(other.attempts, 1);
}

#[tokio::test]
async fn a_resolved_exception_makes_room_for_a_new_one() {
    let db = migrated_db().await;
    let register = register(TestClock::new());
    let tenant = Uuid::from_u128(1);
    let scope = AccessScope::for_tenant(tenant);
    let k = key(tenant, "awaiting_attribution");

    let first = record(&db, &register, &scope, &k, "d").await.unwrap();
    assert!(resolve(&db, &register, &scope, &k).await.unwrap());
    assert!(!resolve(&db, &register, &scope, &k).await.unwrap());
    let reopened = record(&db, &register, &scope, &k, "d").await.unwrap();
    assert_ne!(reopened.exception_id, first.exception_id);
    assert_eq!(reopened.attempts, 1);
}

#[tokio::test]
async fn another_tenants_scope_neither_records_nor_resolves() {
    let db = migrated_db().await;
    let register = register(TestClock::new());
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let k = key(a, "awaiting_attribution");
    assert!(
        record(&db, &register, &AccessScope::for_tenant(b), &k, "d")
            .await
            .is_err()
    );
    record(&db, &register, &AccessScope::for_tenant(a), &k, "d")
        .await
        .unwrap();
    assert!(
        !resolve(&db, &register, &AccessScope::for_tenant(b), &k)
            .await
            .unwrap()
    );
}
