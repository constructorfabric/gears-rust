//! The licensing surface through real managed admission: the toolkit plugin
//! base, the declarations license-resolver publishes from its SDK, and the
//! plugin instance this gear publishes all admit through the local registry's
//! database/outbox path, and the registry serves what the resolver reads.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use gts::GtsSchema;
use license_resolver_sdk::LicenseResolverPluginSpecV1;
use license_resolver_sdk::gts::{LicenseResourceV1, LicenseSubjectV1};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use static_license_plugin::gear::{INSTANCE_SUFFIX, StaticLicensePlugin};
use toolkit::config::ConfigProvider;
use toolkit::gts::PluginV1;
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
    EntityField, FieldSelection, GtsId, GtsTypeId, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions, ReconcileOptions,
};

const VENDOR: &str = "constructorfabric";
const PRIORITY: i16 = 100;

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
}

async fn harness() -> Harness {
    let dsn = format!(
        "sqlite:file:license-managed-{}?mode=memory&cache=shared",
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
        "static-license-plugin": { "config": { "vendor": VENDOR, "priority": PRIORITY } }
    }))
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

/// The toolkit plugin base the plugin spec derives from, as the registry
/// seeds it.
fn plugin_base() -> (GtsId, Value) {
    let id = "gts.cf.toolkit.plugins.plugin.v1~";
    let schema = toolkit::gts::all_inventory_type_schemas()
        .expect("inventory")
        .into_iter()
        .find(|schema| schema["$id"] == format!("gts://{id}"))
        .expect("the toolkit plugin base is declared");
    (GtsId::try_new(id).expect("id"), schema)
}

/// What license-resolver publishes: its SDK collector.
fn resolver_declarations() -> Vec<(GtsId, Value)> {
    license_resolver_sdk::gts_declarations()
        .into_iter()
        .map(|(id, text)| {
            (
                GtsId::try_new(&id).expect("id"),
                serde_json::from_str(&text).expect("declared JSON"),
            )
        })
        .collect()
}

/// The licensing bases, the plugin spec and this gear's instance all admit,
/// and the instance advertises its vendor and priority.
#[tokio::test]
async fn the_licensing_surface_admits_through_managed_admission() {
    let h = harness().await;
    let manifest = StaticLicensePlugin::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("plugin manifest");

    admit(&h, "toolkit", &[plugin_base()]).await;
    admit(&h, "license-resolver", &resolver_declarations()).await;
    admit(&h, "static-license-plugin", manifest.documents()).await;

    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let ctx = PlatformSecurityContext::outbound_marker();
    for type_id in [
        <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID,
        <LicenseResourceV1<()> as GtsSchema>::TYPE_ID,
        LicenseResolverPluginSpecV1::TYPE_ID,
    ] {
        api.get_type_schema(&ctx, &GtsTypeId::new(type_id), ReadOptions::default())
            .await
            .unwrap_or_else(|e| panic!("{type_id} is admitted: {e:?}"));
    }

    let (instance_id, _) = PluginV1::<LicenseResolverPluginSpecV1>::build_registration(
        INSTANCE_SUFFIX,
        VENDOR.to_owned(),
        PRIORITY,
    )
    .expect("plugin instance");
    let instance = api
        .get_instance(
            &ctx,
            &instance_id,
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
            ]))),
        )
        .await
        .expect("the plugin instance is admitted");
    let content = instance.content.expect("content");
    assert_eq!(content["vendor"], VENDOR);
    assert_eq!(content["priority"], i64::from(PRIORITY));
}

/// The Resource base reaches the registry carrying its trait schema: the
/// resolver reads `admitted_subjects` off the materialized traits.
#[tokio::test]
async fn the_admitted_resource_base_carries_the_admitted_subjects_trait() {
    let h = harness().await;
    admit(&h, "toolkit", &[plugin_base()]).await;
    admit(&h, "license-resolver", &resolver_declarations()).await;

    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let base = api
        .get_type_schema(
            &PlatformSecurityContext::outbound_marker(),
            &GtsTypeId::new(<LicenseResourceV1<()> as GtsSchema>::TYPE_ID),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::EffectiveTraits,
            ]))),
        )
        .await
        .expect("the Resource licensing base is admitted");
    let content = base.content.expect("content");
    assert!(
        content
            .pointer("/x-gts-traits-schema/properties/admitted_subjects")
            .is_some(),
        "the Resource base must declare admitted_subjects: {content:#}"
    );
    // What the resolver reads: the materialized default, not the declaration.
    let traits = base.effective_traits.expect("effective traits");
    assert_eq!(
        traits.get("admitted_subjects"),
        Some(&serde_json::json!([])),
        "{traits:#}"
    );
}
