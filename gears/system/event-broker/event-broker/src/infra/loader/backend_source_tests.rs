//! What the loader's source answers when the topic cannot be resolved.

use std::sync::Arc;

use memory_event_broker_plugin::MemoryBackend;
use toolkit_gts::GtsInstanceId;

use super::backend_source::BackendEventSource;
use crate::domain::model::Sequence;
use crate::domain::streaming::source::PartitionKey;
use crate::infra::loader::heads::HeadSource;
use crate::infra::loader::source::{EventSource, SourceError};
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::test_support::gear_db::ScratchSqlite;
use crate::test_support::type_registry::registry_holding;
use crate::test_support::{SingleBackendResolver, StaticTypesRegistry};

const TOPIC_ID: &str =
    toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.backendsource.topic.v1");
const MISSING_TOPIC_ID: &str =
    toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.backendsource.missing.v1");

/// A source over a fresh `SQLite` gear database holding `TOPIC_ID`, and that
/// database.
async fn source() -> (BackendEventSource, ScratchSqlite) {
    let scratch = ScratchSqlite::open("cf-eb-backendsource-test").await;
    let db = Arc::clone(scratch.db());
    let client: Arc<dyn types_registry_sdk::PlatformTypesRegistryApi> = registry_holding(
        &[serde_json::json!({ "id": TOPIC_ID, "description": "a topic the loader reads" })],
        &[],
    );
    crate::infra::specification::bulk_load(&client, &db, &StaticTypesRegistry::empty_config())
        .await
        .expect("bulk_load");
    let source = BackendEventSource::new(
        Arc::new(TypesRegistrySpecificationManager::new(db)),
        Arc::new(SingleBackendResolver::new(Arc::new(MemoryBackend::new()))),
    );
    (source, scratch)
}

#[tokio::test]
async fn a_loader_read_during_a_spec_cache_fault_is_unavailable() {
    let (source, scratch) = source().await;
    let dsn = scratch.dsn().to_owned();
    crate::test_support::tables::drop_table(&dsn, "event_broker_spec_cache").await;
    let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();

    let read = source
        .read(&PartitionKey::new(topic.clone(), 0), Sequence::NONE, 10)
        .await;
    let Err(read) = read else {
        panic!("a read must not succeed over an unreadable cache: {read:?}");
    };
    assert_eq!(read, SourceError::Unavailable);
    let Err(watched) = source.watch(&topic, &[0]).await else {
        panic!("a watch must not open over an unreadable cache");
    };
    assert_eq!(watched, SourceError::Unavailable);
}

#[tokio::test]
async fn a_loader_read_of_an_unregistered_topic_fails() {
    let (source, _scratch) = source().await;
    let topic = GtsInstanceId::try_new(MISSING_TOPIC_ID).unwrap();

    let read = source
        .read(&PartitionKey::new(topic.clone(), 0), Sequence::NONE, 10)
        .await;
    let Err(read) = read else {
        panic!("a read must not succeed on an unregistered topic: {read:?}");
    };
    assert_eq!(
        read,
        SourceError::Failed(format!("unknown topic {MISSING_TOPIC_ID}"))
    );
    let Err(watched) = source.watch(&topic, &[0]).await else {
        panic!("a watch must not open on an unregistered topic");
    };
    assert_eq!(
        watched,
        SourceError::Failed(format!("unknown topic {MISSING_TOPIC_ID}"))
    );
}
