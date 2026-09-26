//! A `Storage` with neither handle wired is exactly the state the gear is in
//! between `init()` and the point `serve()` finishes wiring, which is the
//! window these tests are about. The check never reads the database - it only
//! asks which handles are present - but `Storage` needs one to exist.

use std::sync::Arc;

use toolkit::{Healthcheck, HealthcheckStatus};
use toolkit_db::DBProvider;
use uuid::Uuid;

use super::EventBrokerReadiness;
use crate::config::DeploymentMode;
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::infra::storage::Storage;
use crate::infra::storage::migrations::Migrator;

const TEST_INSTANCE_ID: Uuid = Uuid::from_u128(0x5eed);

/// Neither `set_outbox` nor `set_cache`: the state `serve()` has yet to leave.
async fn unwired_storage() -> Arc<Storage> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-eb-health-test-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let opts = toolkit_db::ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = toolkit_db::connect_db(&format!("sqlite://{file}?mode=rwc"), opts)
        .await
        .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        <Migrator as sea_orm_migration::MigratorTrait>::migrations(),
    )
    .await
    .expect("migrations");
    let db = Arc::new(DBProvider::new(db));
    let spec_manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
    Arc::new(Storage::new(db, Arc::new(spec_manager), TEST_INSTANCE_ID))
}

#[tokio::test]
async fn an_ingest_instance_is_not_ready_until_the_outbox_is_wired() {
    let storage = unwired_storage().await;
    let check = EventBrokerReadiness::new(storage, DeploymentMode::ClusterIngest);

    let result = check.check().await;

    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(
        result.message.as_deref(),
        Some("ingest outbox pipeline not started")
    );
    assert_eq!(result.code.as_deref(), Some("starting"));
}

#[tokio::test]
async fn a_delivery_instance_is_not_ready_until_the_cluster_cache_is_wired() {
    let storage = unwired_storage().await;
    let check = EventBrokerReadiness::new(storage, DeploymentMode::ClusterDelivery);

    let result = check.check().await;

    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(result.message.as_deref(), Some("cluster cache not wired"));
    assert_eq!(result.code.as_deref(), Some("starting"));
}

#[tokio::test]
async fn standalone_reports_the_outbox_first_since_it_needs_both() {
    let storage = unwired_storage().await;
    let check = EventBrokerReadiness::new(storage, DeploymentMode::Standalone);

    let result = check.check().await;

    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(
        result.message.as_deref(),
        Some("ingest outbox pipeline not started")
    );
}

#[tokio::test]
async fn a_dispatcher_instance_needs_neither_handle_and_is_ready_at_once() {
    // The mode a check gating on both handles regardless of role would keep
    // out of rotation forever: it installs neither.
    let storage = unwired_storage().await;
    let check = EventBrokerReadiness::new(storage, DeploymentMode::ClusterDispatcher);

    let result = check.check().await;

    assert_eq!(result.status, HealthcheckStatus::Healthy);
    assert_eq!(result.message, None);
}

#[tokio::test]
async fn a_delivery_instance_is_ready_once_the_cluster_cache_is_wired() {
    let storage = unwired_storage().await;
    let (hub, _cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let cache = crate::domain::cluster::EventBrokerCluster::resolve(&hub)
        .await
        .expect("cluster resolves")
        .cache;
    storage.set_cache(cache);

    let result = EventBrokerReadiness::new(storage, DeploymentMode::ClusterDelivery)
        .check()
        .await;

    assert_eq!(result.status, HealthcheckStatus::Healthy);
    assert_eq!(result.message, None);
}
