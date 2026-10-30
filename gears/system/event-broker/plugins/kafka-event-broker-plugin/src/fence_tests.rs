#![allow(clippy::expect_used, reason = "tests")]

use std::time::Duration;

use event_broker_sdk::backend::FenceKey;

use crate::fence::Producers;

#[tokio::test]
async fn the_pool_never_holds_more_keys_than_its_capacity() {
    let producers = Producers::new(1);
    let first = producers.slot(FenceKey::new(1)).await.expect("a slot");
    let waiting =
        tokio::time::timeout(Duration::from_millis(50), producers.slot(FenceKey::new(2))).await;
    assert!(
        waiting.is_err(),
        "a second key got a slot while the first was busy"
    );
    assert_eq!(producers.len(), 1);

    let second = {
        let producers = &producers;
        let next = async move { producers.slot(FenceKey::new(2)).await };
        let release = async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(first);
        };
        tokio::join!(next, release).0
    };
    assert!(
        second.is_some(),
        "the second key got no slot once the first was free"
    );
    assert_eq!(producers.len(), 1);
}

#[tokio::test]
async fn the_same_key_shares_its_slot() {
    let producers = Producers::new(1);
    let one = producers.slot(FenceKey::new(1)).await.expect("a slot");
    let again = producers
        .slot(FenceKey::new(1))
        .await
        .expect("the same slot");
    assert!(std::sync::Arc::ptr_eq(one.slot(), again.slot()));
}
