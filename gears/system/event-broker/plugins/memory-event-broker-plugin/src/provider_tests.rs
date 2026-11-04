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

fn single_process() -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert("single_process".to_owned(), serde_json::Value::Bool(true));
    map
}

#[tokio::test]
async fn memory_refuses_cluster_roles_without_single_process() {
    for role in [Role::Ingest, Role::Delivery] {
        let built = MemoryBackendProvider::default()
            .build(&BuildContext::new(&serde_json::Map::new(), role))
            .await;
        let Err(event_broker_sdk::backend::BackendError::InvalidConfig { detail }) = built else {
            panic!(
                "{role:?}: expected InvalidConfig, got {:?}",
                built.map(|_| ())
            );
        };
        assert_eq!(
            detail,
            "the memory backend holds events in this process only; outside `standalone` it needs \
             `single_process: true`, for tests that run every role in one process",
            "{role:?}"
        );
    }
}

#[tokio::test]
async fn memory_builds_in_cluster_roles_with_single_process() {
    for role in [Role::Ingest, Role::Delivery] {
        let backend = MemoryBackendProvider::default()
            .build(&BuildContext::new(&single_process(), role))
            .await
            .unwrap_or_else(|err| panic!("{role:?}: builds with `single_process: true`: {err}"));
        assert_eq!(
            backend.provision(&spec(), &claim()).await.unwrap(),
            Provisioning::Ready { partitions: 2 },
            "{role:?}"
        );
    }
}

#[tokio::test]
async fn memory_builds_standalone_by_default() {
    let backend = MemoryBackendProvider::default()
        .build(&BuildContext::new(
            &serde_json::Map::new(),
            Role::Standalone,
        ))
        .await
        .unwrap_or_else(|err| panic!("standalone builds from empty settings: {err}"));
    assert_eq!(
        backend.provision(&spec(), &claim()).await.unwrap(),
        Provisioning::Ready { partitions: 2 }
    );
}
