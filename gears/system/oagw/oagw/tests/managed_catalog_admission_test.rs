#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The OAGW catalog as managed registry entities (T38b OAGW slice).
//!
//! Admits the manifest the runtime publishes for OAGW through the real local
//! client and database/outbox admission, then admits an upstream and a route
//! instance shaped like the ones OAGW provisions from.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use toolkit::ConfigProvider;
use toolkit::gts_publication::GtsPublicationCapability;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    GtsId, GtsIdPattern, LifecycleStatus, ListEntitiesRequest, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, PublisherContext, ReconcileOptions,
};

use oagw::OutboundApiGatewayGear;

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
}

async fn harness() -> Harness {
    let dsn = format!(
        "sqlite:file:oagw-managed-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect admission database");
    let mut migrations = types_registry::infra::storage::Migrator::migrations();
    migrations.extend(
        toolkit_db::outbox::outbox_migrations_with_prefix(
            types_registry::infra::outbox::TABLE_PREFIX,
        )
        .expect("registry outbox prefix"),
    );
    run_migrations_for_testing(&db, migrations)
        .await
        .expect("registry and outbox migrations");
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        Arc::new(types_registry::infra::storage::Repos),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        Arc::new(types_registry::domain::ports::metrics::NoopMetrics),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Harness {
        client: LocalClient::new(service),
        _outbox: Box::new(outbox),
    }
}

struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        None
    }
}

/// The documents the runtime publishes for OAGW before its `init`.
fn oagw_manifest() -> Vec<(GtsId, Value)> {
    OutboundApiGatewayGear::default()
        .descriptor()
        .build_gts_manifest(&NoConfig)
        .expect("the OAGW manifest builds")
        .documents()
        .to_vec()
}

async fn admit(h: &Harness, desired: &[(GtsId, Value)]) -> anyhow::Result<()> {
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    api.reconcile_entities_and_await(
        &PlatformSecurityContext::outbound_marker(),
        &PublisherContext {
            name: "oagw-managed-catalog-test".to_owned(),
            version: "0.1.0".parse().expect("version"),
        },
        desired,
        &ReconcileOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// `"type <id>"` / `"instance <id>"` of every active OAGW entity, sorted.
async fn active_oagw_ids(h: &Harness) -> Vec<String> {
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let ctx = PlatformSecurityContext::outbound_marker();
    let query =
        || ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.cf.core.oagw.*").unwrap());
    let types = api
        .list_type_schemas(&ctx, query())
        .await
        .expect("list types");
    let instances = api
        .list_instances(&ctx, query())
        .await
        .expect("list instances");
    let mut ids: Vec<String> = types
        .iter()
        .filter(|t| t.lifecycle_status == LifecycleStatus::Active)
        .map(|t| format!("type {}", t.type_id.as_ref()))
        .chain(
            instances
                .iter()
                .filter(|i| i.lifecycle_status == LifecycleStatus::Active)
                .map(|i| format!("instance {}", i.id.as_ref())),
        )
        .collect();
    ids.sort();
    ids
}

/// Every Type Schema and Instance OAGW publishes is admitted and active.
#[tokio::test]
async fn the_oagw_manifest_is_admitted() {
    let h = harness().await;
    let manifest = oagw_manifest();
    assert_eq!(manifest.len(), 23, "7 Type Schemas and 16 Instances");

    admit(&h, &manifest).await.expect("the OAGW catalog admits");

    let mut expected: Vec<String> = manifest
        .iter()
        .map(|(id, _)| {
            let kind = if id.is_type() { "type" } else { "instance" };
            format!("{kind} {}", id.as_ref())
        })
        .collect();
    expected.sort();
    assert_eq!(active_oagw_ids(&h).await, expected);
}

/// The upstream and route schemas stay open: an instance carrying OAGW's
/// payload fields is admitted under a named (non-UUID) identifier.
#[tokio::test]
async fn upstream_and_route_payloads_admit_under_the_published_schemas() {
    let h = harness().await;
    admit(&h, &oagw_manifest())
        .await
        .expect("the OAGW catalog admits");
    let upstream = gts_id!("cf.core.oagw.upstream.v1~cf.test.oagw.models.v1");
    let route = gts_id!("cf.core.oagw.route.v1~cf.test.oagw.models.v1");

    admit(
        &h,
        &[
            (
                GtsId::try_new(upstream).unwrap(),
                json!({
                    "id": upstream,
                    "server": { "endpoints": [{ "host": "127.0.0.1", "port": 8080, "scheme": "http" }] },
                    "protocol": gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"),
                    "alias": "models",
                    "tags": ["test"]
                }),
            ),
            (
                GtsId::try_new(route).unwrap(),
                json!({
                    "id": route,
                    "upstream_id": upstream,
                    "match": { "http": { "methods": ["GET"], "path": "/v1/models" } }
                }),
            ),
        ],
    )
    .await
    .expect("upstream and route instances admit");
}
