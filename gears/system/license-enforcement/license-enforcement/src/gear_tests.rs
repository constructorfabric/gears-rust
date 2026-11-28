#![allow(clippy::expect_used)]

use license_resolver_sdk::LicenseResolverPluginSpecV1;
use std::sync::Arc;

use serde_json::{Value, json};
use toolkit::config::ConfigProvider;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationCapability;

use super::{INSTANCE_SUFFIX, LicenseEnforcement};

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

/// The runtime publishes the SDK's recipient projection and this gear's
/// license-resolver backend instance, with the configured vendor and
/// priority, before `init`.
#[test]
fn the_manifest_is_the_recipient_projection_and_the_configured_backend_instance() {
    let config = Config(json!({
        "license-enforcement": { "config": { "vendor": "acme", "priority": 7 } }
    }));
    let manifest = LicenseEnforcement::default()
        .descriptor()
        .build_gts_manifest(&config)
        .expect("manifest");

    let (expected, document) =
        PluginV1::<LicenseResolverPluginSpecV1>::build_registration(INSTANCE_SUFFIX, "acme", 7)
            .expect("backend instance");
    let documents = manifest.documents();
    let ids: Vec<&str> = documents.iter().map(|(id, _)| id.as_ref()).collect();
    let mut want = vec![
        expected.as_ref(),
        "gts.cf.core.license_enforcement.recipient.v1~",
    ];
    want.sort_unstable();
    assert_eq!(ids, want);
    let backend = documents
        .iter()
        .find(|(id, _)| id.as_ref() == expected.as_ref())
        .expect("backend instance");
    assert_eq!(backend.1, document);
    assert_eq!(manifest.publisher_name(), "license-enforcement");
}

/// An invalid configuration is never advertised as a backend.
#[test]
fn an_invalid_configuration_publishes_nothing() {
    let config = Config(json!({
        "license-enforcement": { "config": { "idempotency_window_secs": 0 } }
    }));
    assert!(
        LicenseEnforcement::default()
            .descriptor()
            .build_gts_manifest(&config)
            .is_err()
    );
}

fn init_config() -> Config {
    Config(json!({ "license-enforcement": { "config": { "vendor": "acme", "priority": 7 } } }))
}

/// A context for `init`: the configured backend, an allowing PDP and a
/// migrated database, without the admitted manifest.
async fn init_ctx(hub: &Arc<toolkit::ClientHub>) -> toolkit::GearCtx {
    use toolkit::DatabaseCapability as _;

    let db = toolkit_db::connect_db(
        &format!(
            "sqlite:file:le_init_{}?mode=memory&cache=shared",
            uuid::Uuid::now_v7().simple()
        ),
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("in-memory sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        LicenseEnforcement::default().migrations(),
    )
    .await
    .expect("migrations");
    toolkit::GearCtx::new(
        LicenseEnforcement::MODULE_NAME,
        uuid::Uuid::from_u128(1),
        std::sync::Arc::new(init_config()),
        Arc::clone(hub),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db))
}

fn hub() -> Arc<toolkit::ClientHub> {
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(
        crate::test_support::AllowAuthZ,
    ));
    hub
}

fn bound(hub: &toolkit::ClientHub) -> bool {
    let (id, _) =
        PluginV1::<LicenseResolverPluginSpecV1>::build_registration(INSTANCE_SUFFIX, "acme", 7)
            .expect("backend instance");
    hub.try_get_scoped::<dyn license_resolver_sdk::LicenseResolverPluginClient>(
        &toolkit::client_hub::ClientScope::gts_id(id.as_ref()),
    )
    .is_some()
}

/// `init` binds the license-resolver backend client to the admitted instance.
/// Without the admitted manifest it fails before setting anything up, so the
/// runtime's `init` then succeeds.
#[tokio::test]
async fn init_binds_the_admitted_backend_instance() {
    use toolkit::Gear as _;

    let hub = hub();
    let gear = LicenseEnforcement::default();
    let ctx = init_ctx(&hub).await;
    let err = gear.init(&ctx).await.expect_err("no admitted manifest");
    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub));
    assert!(
        hub.get::<dyn license_enforcement_sdk::LicenseEnforcementApi>()
            .is_err(),
        "nothing is registered"
    );

    let manifest = LicenseEnforcement::default()
        .descriptor()
        .build_gts_manifest(&init_config())
        .expect("manifest");
    let ctx = ctx.with_gts_publication_manifest_for_test(Arc::new(manifest));
    gear.init(&ctx).await.expect("init with the manifest");
    assert!(bound(&hub));
}
