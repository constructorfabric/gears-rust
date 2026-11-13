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
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationCapability;

use super::{INSTANCE_SEGMENT, StoragePluginGear};
use crate::infra::storage::Migrator;
use crate::test_support::{draft, tenant};

fn hub() -> Arc<ClientHub> {
    Arc::new(ClientHub::new())
}

/// The plugin instance the runtime publishes for `vendor` (priority 0, the
/// configuration default).
fn instance_id(vendor: &str) -> String {
    let (id, _) = PluginV1::<QuotaEnforcementStoragePluginSpecV1>::build_registration(
        INSTANCE_SEGMENT,
        vendor,
        0,
    )
    .expect("plugin instance serializes");
    id.as_ref().to_owned()
}

fn bound(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn QuotaEnforcementStoragePluginV1>(&ClientScope::gts_id(id))
        .is_some()
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

fn config(vendor: &str) -> StaticConfigProvider {
    StaticConfigProvider {
        root: json!({ "quota-enforcement-storage-plugin": { "config": { "vendor": vendor } } }),
    }
}

/// A context with the manifest the runtime admits before `init`.
async fn make_ctx_over(vendor: &str, with_db: bool, hub: Arc<ClientHub>) -> GearCtx {
    let ctx = unpublished_ctx_over(vendor, with_db, hub).await;
    match StoragePluginGear::default()
        .descriptor()
        .build_gts_manifest(&config(vendor))
    {
        Ok(manifest) => ctx.with_gts_publication_manifest_for_test(Arc::new(manifest)),
        // An invalid configuration builds no manifest; `init` reports it.
        Err(_) => ctx,
    }
}

/// As [`make_ctx_over`], without the admitted manifest.
async fn unpublished_ctx_over(vendor: &str, with_db: bool, hub: Arc<ClientHub>) -> GearCtx {
    let ctx = GearCtx::new(
        StoragePluginGear::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(config(vendor)),
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

/// The manifest is the configured instance, without `init`.
#[test]
fn the_manifest_is_the_configured_plugin_instance() {
    let manifest = StoragePluginGear::default()
        .descriptor()
        .build_gts_manifest(&config("acme"))
        .expect("manifest");

    let ids: Vec<&str> = manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_ref())
        .collect();
    assert_eq!(ids, [instance_id("acme").as_str()]);
    assert!(ids[0].ends_with(INSTANCE_SEGMENT), "{}", ids[0]);
    assert_eq!(
        manifest.publisher_name(),
        "quota-enforcement-storage-plugin"
    );
}

/// Outside the runtime — without an admitted manifest — `init` fails, binds
/// nothing and builds nothing, so the runtime's `init` then succeeds.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let hub = hub();
    let gear = StoragePluginGear::default();

    let err = gear
        .init(&unpublished_ctx_over("acme", true, Arc::clone(&hub)).await)
        .await
        .expect_err("no admitted manifest");

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub, &instance_id("acme")));
    assert!(gear.plugin().is_none(), "nothing is built");

    gear.init(&make_ctx_over("acme", true, Arc::clone(&hub)).await)
        .await
        .expect("init with the manifest");
    assert!(bound(&hub, &instance_id("acme")));
}

/// An invalid configuration is never advertised: it builds no manifest.
#[test]
fn an_invalid_configuration_publishes_nothing() {
    let err = StoragePluginGear::default()
        .descriptor()
        .build_gts_manifest(&config("  "))
        .expect_err("a blank vendor is invalid");
    assert!(format!("{err:#}").contains("vendor"), "{err:#}");
}

/// `init` binds the scoped client to the admitted instance, and the client
/// answers the contract.
#[tokio::test]
async fn init_binds_the_admitted_instance_whose_scoped_client_answers_the_contract() {
    let hub = hub();
    let gear = StoragePluginGear::default();
    gear.init(&make_ctx_over("acme", true, Arc::clone(&hub)).await)
        .await
        .expect("init");

    let client = hub
        .try_get_scoped::<dyn QuotaEnforcementStoragePluginV1>(&ClientScope::gts_id(&instance_id(
            "acme",
        )))
        .expect("the scoped client is bound under the admitted instance");
    client
        .bootstrap(&BootstrapBundle::foundation())
        .await
        .expect("the bound client answers the contract");
}
