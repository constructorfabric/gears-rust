#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;
use toolkit_db::Db;

use super::*;
use crate::infra::storage::repo::operation_repo::OrmOperationRepo;
use crate::test_support::{TestClock, migrated_db};

fn key(tenant: Uuid, actor: Uuid, k: &str) -> OperationKey {
    OperationKey {
        tenant_id: tenant,
        actor,
        idempotency_key: IdempotencyKey::parse(k).unwrap(),
    }
}

/// The operation store of one tenant, each call in a transaction of its own (as a command's).
struct Harness {
    db: Db,
    keys: OperationKeys,
    scope: AccessScope,
}

impl Harness {
    async fn new(clock: Arc<TestClock>) -> Self {
        Self {
            db: migrated_db().await,
            keys: OperationKeys::new(Arc::new(OrmOperationRepo), clock),
            scope: AccessScope::for_tenant(Uuid::from_u128(1)),
        }
    }

    async fn settle(
        &self,
        key: &OperationKey,
        digest: &Sha256Hex,
    ) -> Result<Settlement, DomainError> {
        let (keys, scope, key, digest) = (
            self.keys.clone(),
            self.scope.clone(),
            key.clone(),
            digest.clone(),
        );
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { keys.settle(tx, &scope, &key, &digest).await })
            })
            .await
    }

    /// Records `pending` in a transaction of its own.
    async fn record(
        &self,
        pending: PendingOperation,
        operation_id: Uuid,
        response: Value,
    ) -> Result<StoredOperation, DomainError> {
        let (keys, scope) = (self.keys.clone(), self.scope.clone());
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    keys.record(tx, &scope, pending, operation_id, response)
                        .await
                })
            })
            .await
    }

    /// A command under `key`: settles it and, when it is fresh, records the receipt.
    async fn run(
        &self,
        key: &OperationKey,
        digest: &Sha256Hex,
        operation_id: Uuid,
        response: Value,
    ) -> StoredOperation {
        let pending = fresh(self.settle(key, digest).await.unwrap());
        self.record(pending, operation_id, response).await.unwrap()
    }
}

fn fresh(settlement: Settlement) -> PendingOperation {
    match settlement {
        Settlement::Fresh(pending) => pending,
        Settlement::Replay(stored) => panic!("expected a fresh key, got a replay of {stored:?}"),
    }
}

const TENANT: Uuid = Uuid::from_u128(1);

#[test]
fn idempotency_keys_are_visible_ascii_up_to_255() {
    assert!(IdempotencyKey::parse("abc-123_XYZ.~").is_ok());
    for bad in ["", "a b", "na\u{ef}ve", "\u{7f}"] {
        assert!(IdempotencyKey::parse(bad).is_err(), "{bad:?}");
    }
    assert!(IdempotencyKey::parse(&"k".repeat(256)).is_err());
}

#[tokio::test]
async fn a_retry_with_the_same_body_returns_the_original_receipt() {
    let h = Harness::new(TestClock::new()).await;
    let k = key(TENANT, Uuid::from_u128(9), "retry-me");
    let digest = Sha256Hex::of_json(&json!({"selector": "s"}));

    let pending = fresh(h.settle(&k, &digest).await.unwrap());
    assert_eq!(pending.key(), &k);
    let operation = Uuid::from_u128(77);
    let receipt = json!({"run_id": operation, "state": "accepted"});
    h.record(pending, operation, receipt.clone()).await.unwrap();

    match h.settle(&k, &digest).await.unwrap() {
        Settlement::Replay(stored) => {
            assert_eq!(stored.operation_id, operation);
            assert_eq!(stored.response, receipt);
        }
        Settlement::Fresh(_) => panic!("a retry must replay"),
    }
}

#[tokio::test]
async fn the_same_key_with_another_body_is_refused() {
    let h = Harness::new(TestClock::new()).await;
    let k = key(TENANT, Uuid::from_u128(9), "k");
    let first = Sha256Hex::of_json(&json!({"n": 1}));
    h.run(&k, &first, Uuid::from_u128(1), json!({})).await;
    let other = Sha256Hex::of_json(&json!({"n": 2}));
    assert_eq!(
        h.settle(&k, &other).await,
        Err(DomainError::IdempotencyKeyReused {
            key: "k".to_owned()
        })
    );
}

#[tokio::test]
async fn a_key_is_scoped_by_tenant_and_actor() {
    let h = Harness::new(TestClock::new()).await;
    let digest = Sha256Hex::of_json(&json!({"n": 1}));
    h.run(
        &key(TENANT, Uuid::from_u128(9), "k"),
        &digest,
        Uuid::from_u128(1),
        json!({}),
    )
    .await;
    let other_actor = key(TENANT, Uuid::from_u128(8), "k");
    let other_digest = Sha256Hex::of_json(&json!({"n": 2}));
    assert!(matches!(
        h.settle(&other_actor, &other_digest).await.unwrap(),
        Settlement::Fresh(_)
    ));
}

#[tokio::test]
async fn a_key_is_kept_seven_days() {
    let clock = TestClock::new();
    let h = Harness::new(Arc::clone(&clock)).await;
    let k = key(TENANT, Uuid::from_u128(9), "k");
    let digest = Sha256Hex::of_json(&json!({"n": 1}));
    h.run(&k, &digest, Uuid::from_u128(1), json!({})).await;

    clock.advance(OPERATION_KEY_RETENTION - Duration::seconds(1));
    assert!(matches!(
        h.settle(&k, &digest).await.unwrap(),
        Settlement::Replay(_)
    ));
    clock.advance(Duration::seconds(1));
    let other = Sha256Hex::of_json(&json!({"n": 2}));
    let pending = fresh(h.settle(&k, &other).await.unwrap());
    // The expired key was removed, so the key can be recorded again.
    h.record(pending, Uuid::from_u128(2), json!({}))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_concurrent_record_of_the_same_key_is_a_lost_race() {
    let h = Harness::new(TestClock::new()).await;
    let k = key(TENANT, Uuid::from_u128(9), "k");
    let digest = Sha256Hex::of_json(&json!({"n": 1}));
    // Two requests settle the key before either records it.
    let first = fresh(h.settle(&k, &digest).await.unwrap());
    let second = fresh(h.settle(&k, &digest).await.unwrap());
    h.record(first, Uuid::from_u128(1), json!({}))
        .await
        .unwrap();
    assert!(matches!(
        h.record(second, Uuid::from_u128(2), json!({})).await,
        Err(DomainError::Concurrent(_))
    ));
}
