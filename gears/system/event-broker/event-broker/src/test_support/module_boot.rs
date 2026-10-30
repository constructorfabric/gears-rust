//! Booting the gear itself: a `GearCtx` the way the platform hands one to
//! `EventBrokerModule::init`, and `serve()` on a task of its own. For the
//! module tests in this crate and for the engine-matrix tests that boot a
//! cluster mode on a server gear database.

use std::sync::Arc;

use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, GearCtx};
use uuid::Uuid;

use super::GearDb;
use crate::EventBrokerModule;

/// The platform's configuration, fixed to one JSON document.
pub struct StaticConfigProvider {
    pub root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

/// `gear_db` opened and migrated, as the platform hands it to `init()`.
pub async fn migrated_gear_db(gear_db: &GearDb) -> toolkit_db::DBProvider<toolkit_db::DbError> {
    (*gear_db.migrated().await.db).clone()
}

/// A context for `mode`, with `db` attached and `extra` merged into the
/// `event-broker` config over a single memory backend routed for every topic.
///
/// Registers an empty `MockTypesRegistryClient` on `hub` (a test that seeds
/// one registers its own afterwards), and binds a `standalone` cache provider
/// under the `event-broker` cluster profile unless `hub` already resolves one.
pub async fn module_ctx(
    mode: &str,
    hub: Arc<ClientHub>,
    extra: &serde_json::Value,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
) -> GearCtx {
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        types_registry_sdk::testing::MockTypesRegistryClient::new(),
    ));
    if crate::domain::cluster::EventBrokerCluster::resolve(&hub)
        .await
        .is_err()
    {
        let config: cluster::ClusterConfig = serde_json::from_value(json!({
            "profiles": { "event-broker": { "cache": { "provider": "standalone" } } }
        }))
        .expect("valid test cluster config");
        let providers = cluster::ProviderRegistry::new()
            .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));
        let handle = cluster::ClusterWiring::from_config(Arc::clone(&hub), &config, &providers)
            .await
            .expect("standalone provider wires cleanly");
        // Intentionally leaked - matches `standalone_event_broker_cluster()`'s
        // own "no graceful shutdown needed in tests" precedent.
        std::mem::forget(handle);
    }
    let mut cfg = json!({
        "event-broker": {
            "config": {
                "mode": mode,
                "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
                "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
                // Only load-bearing for cluster_ingest/cluster_delivery
                // (registration `init`-time validation); harmless for the other
                // two modes.
                "registration": { "advertise_addr": "127.0.0.1:8080" },
            }
        }
    });
    for (key, value) in extra.as_object().expect("extra config is an object") {
        cfg["event-broker"]["config"][key] = value.clone();
    }
    GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        hub,
        CancellationToken::new(),
    )
    .with_db(db)
}

/// Runs `module`'s `serve()` on a task of its own until `cancel` fires.
pub fn spawn_serve(
    module: &Arc<EventBrokerModule>,
    cancel: &CancellationToken,
) -> JoinHandle<anyhow::Result<()>> {
    let module = Arc::clone(module);
    let cancel = cancel.clone();
    tokio::spawn(async move { module.serve(cancel).await })
}
