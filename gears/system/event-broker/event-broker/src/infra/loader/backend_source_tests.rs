//! What the loader's source answers when the topic cannot be resolved.

use std::sync::Arc;

use memory_event_broker_plugin::MemoryBackend;
use toolkit_db::DBProvider;
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use super::backend_source::BackendEventSource;
use crate::domain::model::Sequence;
use crate::domain::streaming::source::PartitionKey;
use crate::infra::loader::heads::HeadSource;
use crate::infra::loader::source::{EventSource, SourceError};
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::infra::storage::migrations::Migrator;
use crate::test_support::{SingleBackendResolver, StaticTypesRegistry};

const TOPIC_ID: &str =
    toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.backendsource.topic.v1");
const MISSING_TOPIC_ID: &str =
    toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.backendsource.missing.v1");

/// A source over a fresh `SQLite` gear database holding `TOPIC_ID`, and a DSN
/// that reaches the same file.
async fn source() -> (BackendEventSource, String) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-eb-backendsource-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
    let opts = toolkit_db::ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = toolkit_db::connect_db(&dsn, opts)
        .await
        .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        <Migrator as sea_orm_migration::MigratorTrait>::migrations(),
    )
    .await
    .expect("migrations");
    let db = Arc::new(DBProvider::new(db));
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_instances(vec![make_test_instance(
            TOPIC_ID,
            serde_json::json!({ "id": TOPIC_ID, "description": "a topic the loader reads" }),
        )]),
    );
    crate::infra::specification::bulk_load(&client, &db, &StaticTypesRegistry::empty_config())
        .await
        .expect("bulk_load");
    let source = BackendEventSource::new(
        Arc::new(TypesRegistrySpecificationManager::new(db)),
        Arc::new(SingleBackendResolver::new(Arc::new(MemoryBackend::new()))),
    );
    (source, dsn)
}

#[tokio::test]
async fn a_loader_read_during_a_spec_cache_fault_is_unavailable() {
    let (source, dsn) = source().await;
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
    let (source, _dsn) = source().await;
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
