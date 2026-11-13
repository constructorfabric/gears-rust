#![allow(clippy::expect_used)]
//! The QE publications as managed registry entities (T38b QE slice): the
//! manifest the runtime publishes for quota-enforcement and for its storage
//! plugin admit through the real local client and database/outbox admission,
//! and owner contracts derived from the QE bases admit on top of them.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit::config::ConfigProvider;
use toolkit::gts_publication::GtsPublicationCapability;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsIdPattern, GtsTypeId, LifecycleStatus,
    ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection,
    PublisherContext, ReadOptions, ReconcileOptions,
};

use crate::QuotaEnforcementGear;
use crate::test_support::{
    LLM_USER_PROJECTION, document_id, llm_gateway_documents, metric_base_documents,
};
use quota_enforcement_storage_plugin::StoragePluginGear;

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
}

async fn harness() -> Harness {
    let dsn = format!(
        "sqlite:file:qe-managed-{}?mode=memory&cache=shared",
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

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

fn config() -> Config {
    Config(serde_json::json!({
        "quota-enforcement-storage-plugin": { "config": { "vendor": "acme" } }
    }))
}

fn documents(manifest: &toolkit::gts_publication::GtsManifest) -> Vec<(GtsId, Value)> {
    manifest.documents().to_vec()
}

/// The toolkit plugin base the plugin specs derive from; the registry seeds
/// it from the toolkit-gts declarations.
fn plugin_base() -> (GtsId, Value) {
    let id = "gts.cf.toolkit.plugins.plugin.v1~";
    let schema = toolkit::gts::all_inventory_type_schemas()
        .expect("inventory")
        .into_iter()
        .find(|schema| document_id(schema) == id)
        .expect("the toolkit plugin base is declared");
    (GtsId::try_new(id).expect("id"), schema)
}

fn as_pairs(documents: Vec<Value>) -> Vec<(GtsId, Value)> {
    documents
        .into_iter()
        .map(|doc| (GtsId::try_new(&document_id(&doc)).expect("id"), doc))
        .collect()
}

async fn admit(h: &Harness, publisher: &str, desired: &[(GtsId, Value)]) {
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    api.reconcile_entities_and_await(
        &PlatformSecurityContext::outbound_marker(),
        &PublisherContext {
            name: publisher.to_owned(),
            version: "0.1.0".parse().expect("version"),
        },
        desired,
        &ReconcileOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{publisher} admits: {e:?}"));
}

/// The QE manifest, the storage plugin instance and the `llm_gateway`
/// contracts over a stand-in metric base all admit; the registry then serves
/// a concrete subject projection with its materialized scope trait.
#[tokio::test]
async fn the_qe_publications_and_owner_contracts_admit() {
    let h = harness().await;
    let qe = QuotaEnforcementGear::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("QE manifest");
    let plugin = StoragePluginGear::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("plugin manifest");

    admit(&h, "toolkit", &[plugin_base()]).await;
    admit(&h, "quota-enforcement", &documents(&qe)).await;
    admit(&h, "quota-enforcement-storage-plugin", &documents(&plugin)).await;
    admit(&h, "metric-owner", &as_pairs(metric_base_documents())).await;
    admit(&h, "llm-gateway", &as_pairs(llm_gateway_documents())).await;

    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let ctx = PlatformSecurityContext::outbound_marker();
    let qe_ids: Vec<String> = documents(&qe)
        .iter()
        .map(|(id, _)| id.as_ref().to_owned())
        .collect();
    let mut active: Vec<String> = api
        .list_type_schemas(
            &ctx,
            ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.*").expect("pattern")),
        )
        .await
        .expect("types")
        .into_iter()
        .filter(|t| t.lifecycle_status == LifecycleStatus::Active)
        .map(|t| t.type_id.as_ref().to_owned())
        .chain(
            api.list_instances(
                &ctx,
                ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.*").expect("pattern")),
            )
            .await
            .expect("instances")
            .into_iter()
            .filter(|i| i.lifecycle_status == LifecycleStatus::Active)
            .map(|i| i.id.as_ref().to_owned()),
        )
        .collect();
    active.sort();
    for id in qe_ids.iter().chain(
        documents(&plugin)
            .iter()
            .map(|(id, _)| id.as_ref().to_owned())
            .collect::<Vec<_>>()
            .iter(),
    ) {
        assert!(active.contains(id), "{id} is admitted and active");
    }

    let user = api
        .get_type_schema(
            &ctx,
            &GtsTypeId::new(LLM_USER_PROJECTION),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::EffectiveTraits,
            ]))),
        )
        .await
        .expect("the user projection");
    assert_eq!(
        user.effective_traits.expect("traits")["scope"],
        quota_enforcement_sdk::SCOPE_USER
    );
}
