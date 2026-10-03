#![allow(clippy::expect_used)]

use std::sync::Arc;

use quota_enforcement_sdk::BootstrapBundle;
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

use quota_enforcement_sdk::{QuotaEnforcementStoragePluginSpecV1, QuotaEnforcementStoragePluginV1};
use toolkit::client_hub::ClientScope;
use types_registry_sdk::{InstanceQuery, RegisterResult, TypesRegistryClient};

use super::{INSTANCE_SEGMENT, StoragePluginGear};
use crate::infra::storage::Migrator;
use crate::test_support::{draft, tenant};

/// A real in-process types-registry seeded with the process inventory (the
/// storage plugin spec among it) and switched to ready, as at run time.
fn in_process_registry() -> Arc<dyn TypesRegistryClient> {
    let config = types_registry::config::TypesRegistryConfig::default();
    let repository = Arc::new(types_registry::infra::InMemoryGtsRepository::new(
        config.to_gts_config(),
    ));
    let service = Arc::new(types_registry::domain::TypesRegistryService::new(
        repository, config,
    ));
    let mut entries = toolkit::gts::all_inventory_type_schemas().expect("inventory type schemas");
    entries.extend(toolkit::gts::all_inventory_instances().expect("inventory instances"));
    RegisterResult::ensure_all_ok(&service.register(entries)).expect("inventory registers");
    service.switch_to_ready().expect("registry ready");
    Arc::new(types_registry::domain::local_client::TypesRegistryLocalClient::new(service))
}

fn hub() -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(in_process_registry());
    hub
}

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

async fn make_ctx(vendor: &str, with_db: bool) -> GearCtx {
    make_ctx_over(vendor, with_db, hub()).await
}

async fn make_ctx_over(vendor: &str, with_db: bool, hub: Arc<ClientHub>) -> GearCtx {
    let cfg = json!({ "quota-enforcement-storage-plugin": { "config": { "vendor": vendor } } });
    let ctx = GearCtx::new(
        StoragePluginGear::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(StaticConfigProvider { root: cfg }),
        hub,
        CancellationToken::new(),
    );
    if !with_db {
        return ctx;
    }
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..ConnectOpts::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    ctx.with_db(DBProvider::new(db))
}

#[tokio::test]
async fn init_binds_the_plugin_to_the_database_and_bootstrap_works_through_it() {
    let gear = StoragePluginGear::default();
    assert!(gear.plugin().is_none());
    gear.init(&make_ctx("acme", true).await)
        .await
        .expect("init succeeds");
    let plugin = gear.plugin().expect("plugin bound");
    let report = plugin
        .bootstrap(&BootstrapBundle::foundation())
        .await
        .expect("bootstrap through the bound plugin");
    assert_eq!(report.inserted, 3);

    // The Quota store is wired over the gear's late-bound outbox: until
    // delivery starts and binds it, a mutation that enqueues an event rolls
    // back as unavailable and nothing is written, while reads answer.
    assert!(!gear.notification_outbox().is_bound());
    let err = plugin
        .create_quota(
            &security_ctx(),
            &toolkit_security::AccessScope::for_tenant(tenant().as_uuid()),
            draft(tenant(), "u1", Some(1)),
            &[crate::test_support::quota_changed(tenant())],
        )
        .await
        .expect_err("outbox unbound");
    assert!(
        matches!(err, quota_enforcement_sdk::StorageError::Unavailable(_)),
        "{err:?}"
    );
    let page = plugin
        .read_quotas(
            &security_ctx(),
            &toolkit_security::AccessScope::allow_all(),
            quota_enforcement_sdk::QuotaFilter::default(),
            quota_enforcement_sdk::PageRequest::default(),
        )
        .await
        .expect("reads need no outbox");
    assert!(page.items.is_empty());
}

fn security_ctx() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

#[tokio::test]
async fn init_fails_without_a_database_binding() {
    let gear = StoragePluginGear::default();
    let err = gear
        .init(&make_ctx("acme", false).await)
        .await
        .expect_err("db capability requires a binding");
    assert!(err.to_string().contains("Database"), "{err}");
    assert!(gear.plugin().is_none());
}

#[tokio::test]
async fn init_fails_on_a_blank_vendor_and_on_a_second_call() {
    let gear = StoragePluginGear::default();
    let err = gear
        .init(&make_ctx("  ", true).await)
        .await
        .expect_err("blank vendor rejected");
    assert!(err.to_string().contains("vendor"), "{err}");

    let ctx = make_ctx("acme", true).await;
    gear.init(&ctx).await.expect("first init");
    let err = gear.init(&ctx).await.expect_err("second init");
    assert!(err.to_string().contains("already initialized"), "{err}");
}

#[tokio::test]
async fn init_publishes_one_plugin_instance_whose_scoped_client_answers_the_contract() {
    let hub = hub();
    let gear = StoragePluginGear::default();
    gear.init(&make_ctx_over("acme", true, Arc::clone(&hub)).await)
        .await
        .expect("init");

    let registry = hub.get::<dyn TypesRegistryClient>().expect("registry");
    let instances = registry
        .list_instances(InstanceQuery::new().with_pattern(format!(
            "{}*",
            <QuotaEnforcementStoragePluginSpecV1 as gts::GtsSchema>::TYPE_ID
        )))
        .await
        .expect("list");
    assert_eq!(instances.len(), 1, "one instance: {instances:?}");
    let instance = &instances[0];
    assert!(
        instance.id.to_string().ends_with(INSTANCE_SEGMENT),
        "{}",
        instance.id
    );
    let client = hub
        .try_get_scoped::<dyn QuotaEnforcementStoragePluginV1>(&ClientScope::gts_id(
            instance.id.as_ref(),
        ))
        .expect("the scoped client is registered under the instance");
    client
        .bootstrap(&BootstrapBundle::foundation())
        .await
        .expect("the published client answers the contract");
}
