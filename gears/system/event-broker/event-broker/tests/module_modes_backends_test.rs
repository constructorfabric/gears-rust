//! The gear booted in a cluster mode and served. A cluster mode refuses a
//! `SQLite` gear database (feature 0005 section 2.4, C10), so these run on
//! `PostgreSQL`.
//!
//! Needs a Docker daemon:
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test module_modes_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use event_broker::EventBrokerModule;
use event_broker::domain::specification::SpecificationManager;
use event_broker::infra::specification::TypesRegistrySpecificationManager;
use event_broker::test_support::module_boot::{migrated_gear_db, module_ctx, spawn_serve};
use event_broker::test_support::{standalone_event_broker_cluster, test_directory_client};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::{ClientHub, Gear};

use support::matrix::{Engine, Matrix};

/// Exactly one role maintains the specification cache. Both halves are
/// asserted against the same seeded registry, because "the dispatcher wrote
/// nothing" only means something next to "the ingest role wrote something".
#[tokio::test]
async fn only_the_ingest_role_maintains_the_specification_cache() {
    const TOPIC_ID: &str = "gts.cf.core.events.topic.v1~x.eb.roles.topic.v1";

    /// `patience` is how long to keep looking. Proving a cache was filled
    /// needs a generous bound under a loaded runtime; proving one was never
    /// filled only needs long enough for a load to have started, since the
    /// initial load is awaited inside `serve()` before anything else is wired.
    async fn topics_after_serve(matrix: &Matrix, mode: &str, patience: Duration) -> usize {
        let hub = Arc::new(ClientHub::new());
        let db = migrated_gear_db(&matrix.gear_db()).await;
        let ctx = module_ctx(mode, Arc::clone(&hub), &json!({}), db).await;
        // After `module_ctx`, which registers an empty one of its own.
        hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
            types_registry_sdk::testing::MockTypesRegistryClient::new().with_instances(vec![
                types_registry_sdk::testing::make_test_instance(
                    TOPIC_ID,
                    json!({ "id": TOPIC_ID, "description": "a topic the roles test seeds" }),
                ),
            ]),
        ));
        let db = Arc::new(ctx.db_required().expect("the test ctx carries a database"));

        let module = Arc::new(EventBrokerModule::default());
        module.init(&ctx).await.expect("init must succeed");
        let cancel = CancellationToken::new();
        let serve_task = spawn_serve(&module, &cancel);

        // The initial load is awaited inside `serve()` before anything else is
        // wired, so a cache that is going to be filled is filled by the time
        // the first poll finds the task still running.
        let deadline = Instant::now() + patience;
        let manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
        let count = loop {
            let found = manager.list_topics().await.len();
            if found > 0 || Instant::now() >= deadline {
                break found;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        cancel.cancel();
        drop(serve_task.await);
        count
    }

    let Some(matrix) = Matrix::start(Engine::Postgres, "module-roles").await else {
        return;
    };
    assert_eq!(
        topics_after_serve(&matrix, "cluster_ingest", Duration::from_secs(5)).await,
        1,
        "the ingest role loads and resolves what types-registry holds"
    );
    assert_eq!(
        topics_after_serve(&matrix, "cluster_dispatcher", Duration::from_millis(250)).await,
        0,
        "a dispatcher forwards requests and holds no specification state"
    );
}

/// Spec "Ingest/delivery self-registration": serving in `cluster_ingest`
/// mode registers with `DirectoryService` under `"event-broker-ingest"` at
/// the resolved advertise address.
#[tokio::test]
async fn cluster_ingest_serve_registers_with_service_discovery() {
    let Some(matrix) = Matrix::start(Engine::Postgres, "module-discovery").await else {
        return;
    };
    let (hub, _cluster) = standalone_event_broker_cluster().await;
    let directory = test_directory_client(&hub);
    let db = migrated_gear_db(&matrix.gear_db()).await;
    let ctx = module_ctx("cluster_ingest", Arc::clone(&hub), &json!({}), db).await;

    let module = Arc::new(EventBrokerModule::default());
    module.init(&ctx).await.expect("init must succeed");

    let cancel = CancellationToken::new();
    let serve_task = spawn_serve(&module, &cancel);

    // Poll rather than sleep a fixed span. `serve()` runs on another task, and
    // how quickly it reaches `register_instance()` depends on how loaded the
    // machine is. The deadline is generous because it only bounds a failure;
    // a healthy run leaves this loop on its first or second pass.
    let deadline = Instant::now() + Duration::from_secs(5);
    let instances = loop {
        let found = directory
            .list_instances("event-broker-ingest")
            .await
            .expect("list_instances must not fail");
        if !found.is_empty() || Instant::now() >= deadline {
            break found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(instances.len(), 1, "instances: {instances:?}");
    assert_eq!(
        instances[0]
            .rest_endpoint
            .as_ref()
            .map(|ep| ep.uri.as_str()),
        Some("http://127.0.0.1:8080")
    );

    cancel.cancel();
    serve_task
        .await
        .expect("serve task must not panic")
        .expect("serve must return Ok on cancellation");
}
