use event_broker_sdk::backend::{
    BuildContext, LogClaim, LogId, Partition, Provider, Provisioning, Replacing, Retention, Role,
    TopicPartition, TopicSpec,
};
use event_broker_sdk::{GtsInstanceId, Position, Sequence};
use uuid::Uuid;

use crate::{BACKEND_TYPE, MemoryBackendProvider};

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1").expect("valid id")
}

fn spec() -> TopicSpec {
    TopicSpec {
        topic: topic(),
        partitions: 2,
        retention: Retention::default(),
    }
}

fn claim() -> LogClaim {
    LogClaim {
        replacing: Replacing::Nothing,
        new: LogId::from_uuid(Uuid::from_u128(7)),
    }
}

fn settings(store: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert("store".to_owned(), serde_json::Value::from(store));
    map
}

#[test]
fn backend_type_is_the_memory_type() {
    assert_eq!(
        MemoryBackendProvider::default().backend_type().as_ref(),
        BACKEND_TYPE
    );
}

#[tokio::test]
async fn entries_naming_one_store_share_it() {
    let provider = MemoryBackendProvider::default();
    let first = provider
        .build(&BuildContext::new(&settings("a"), Role::Standalone))
        .await
        .unwrap();
    let second = provider
        .build(&BuildContext::new(&settings("a"), Role::Standalone))
        .await
        .unwrap();

    assert_eq!(
        first.provision(&spec(), &claim()).await.unwrap(),
        Provisioning::Ready { partitions: 2 }
    );
    let head = second
        .resolve(
            &TopicPartition::new(topic(), Partition::new(0)),
            Position::Latest,
        )
        .await
        .unwrap();
    assert_eq!(head, Sequence::NONE);
}

#[tokio::test]
async fn entries_naming_different_stores_do_not_share() {
    let provider = MemoryBackendProvider::default();
    let first = provider
        .build(&BuildContext::new(&settings("a"), Role::Standalone))
        .await
        .unwrap();
    let second = provider
        .build(&BuildContext::new(&settings("b"), Role::Standalone))
        .await
        .unwrap();

    first.provision(&spec(), &claim()).await.unwrap();
    let missing = second
        .resolve(
            &TopicPartition::new(topic(), Partition::new(0)),
            Position::Latest,
        )
        .await;
    assert!(
        matches!(
            missing,
            Err(event_broker_sdk::backend::BackendError::NotProvisioned { .. })
        ),
        "expected NotProvisioned, got {missing:?}"
    );
}
