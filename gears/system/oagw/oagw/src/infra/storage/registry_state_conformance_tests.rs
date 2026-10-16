//! `RegistryStateStore` conformance: one scenario from a fresh row, on every
//! backend (in memory, `SQLite`, and `PostgreSQL` and `MySQL` behind the
//! `integration` feature). The row is installation-wide, so the scenario runs
//! in order on one store instead of as independent cases.

use std::time::Duration;

use toolkit_db::{DBProvider, Db};
use uuid::Uuid;

use super::registry_state_db_repo::DbRegistryStateStore;
use super::registry_state_in_memory::InMemoryRegistryStateStore;
use super::testing;
use crate::domain::repo::{RegistryState, RegistryStateStore};

const TTL: Duration = Duration::from_secs(60);

fn db_store(db: Db) -> DbRegistryStateStore {
    DbRegistryStateStore::new(DBProvider::new(db))
}

/// ADR-0018: PERS-GATE-003, PERS-GATE-009, PERS-GATE-012.
async fn scenario(store: &dyn RegistryStateStore) {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let state = |generation: Option<u64>, hash: Option<&str>| RegistryState {
        generation,
        content_hash: hash.map(str::to_owned),
    };

    // A fresh row.
    assert_eq!(store.read().await.unwrap(), state(None, None));

    // One holder at a time; the holder may take it again.
    assert!(store.try_acquire(a, TTL, None).await.unwrap());
    assert!(!store.try_acquire(b, TTL, None).await.unwrap());
    assert!(store.try_acquire(a, TTL, None).await.unwrap());
    assert!(store.renew(a, TTL).await.unwrap());
    assert!(!store.renew(b, TTL).await.unwrap());

    // Only the holder records and releases; an unset generation stays unset.
    assert!(!store.finish(b, Some("h1")).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(None, None));
    assert!(store.finish(a, Some("h1")).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(None, Some("h1")));
    assert!(!store.renew(a, TTL).await.unwrap(), "released");

    // The generation only rises.
    assert!(store.claim(5).await.unwrap());
    assert!(!store.claim(5).await.unwrap());
    assert!(!store.claim(3).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(5), Some("h1")));

    // Acquiring refuses an older generation and claims a newer or equal one.
    assert!(!store.try_acquire(a, TTL, Some(4)).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(5), Some("h1")));
    // Taking the lease forgets the recorded hash: a run may stop half-way.
    assert!(store.try_acquire(a, TTL, Some(6)).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(6), None));
    // A refused acquire claims nothing, however new its generation.
    assert!(!store.try_acquire(b, TTL, Some(7)).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(6), None));
    assert!(
        store.finish(a, None).await.unwrap(),
        "release without a hash"
    );
    assert_eq!(store.read().await.unwrap(), state(Some(6), None));
    assert!(store.try_acquire(b, TTL, Some(6)).await.unwrap());
    assert!(store.finish(b, None).await.unwrap());

    // A lapsed lease is taken over, and its former holder can neither renew
    // nor record. The database clock has whole seconds on SQLite.
    assert!(
        store
            .try_acquire(a, Duration::from_secs(1), None)
            .await
            .unwrap()
    );
    assert!(!store.try_acquire(b, TTL, None).await.unwrap());
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    assert!(store.try_acquire(b, TTL, None).await.unwrap(), "expired");
    assert!(!store.renew(a, TTL).await.unwrap());
    assert!(!store.finish(a, Some("h2")).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(6), None));
    assert!(store.finish(b, Some("h2")).await.unwrap());
    assert_eq!(store.read().await.unwrap(), state(Some(6), Some("h2")));
}

#[tokio::test]
async fn in_memory() {
    scenario(&InMemoryRegistryStateStore::new()).await;
}

#[tokio::test]
async fn sqlite() {
    scenario(&db_store(testing::shared_sqlite().await)).await;
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn postgres() {
    let (_container, db) = testing::postgres().await;
    scenario(&db_store(db)).await;
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn mysql() {
    let (_container, db) = testing::mysql().await;
    scenario(&db_store(db)).await;
}
