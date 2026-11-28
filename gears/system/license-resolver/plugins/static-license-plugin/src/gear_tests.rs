use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx};
use uuid::Uuid;

use license_resolver_sdk::{LicenseResolverPluginClient, LicenseResolverPluginSpecV1};
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationCapability;

use super::StaticLicensePlugin;
use crate::config::StaticLicensePluginConfig;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx(hub: Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        StaticLicensePlugin::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(StaticConfigProvider { root: json!({}) }),
        hub,
        CancellationToken::new(),
    )
}

fn instance_id() -> String {
    let (id, _) = PluginV1::<LicenseResolverPluginSpecV1>::build_registration(
        super::INSTANCE_SUFFIX,
        StaticLicensePluginConfig::default().vendor,
        StaticLicensePluginConfig::default().priority,
    )
    .expect("plugin instance");
    id.as_ref().to_owned()
}

fn bound(hub: &ClientHub) -> bool {
    hub.try_get_scoped::<dyn LicenseResolverPluginClient>(&ClientScope::gts_id(&instance_id()))
        .is_some()
}

/// The manifest is the configured instance, without `init`.
#[test]
fn the_manifest_is_the_configured_plugin_instance() {
    let manifest = StaticLicensePlugin::default()
        .descriptor()
        .build_gts_manifest(&StaticConfigProvider { root: json!({}) })
        .expect("manifest");
    let ids: Vec<&str> = manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_ref())
        .collect();
    assert_eq!(ids, [instance_id().as_str()]);
}

/// Outside the runtime — without an admitted manifest — `init` fails and
/// binds nothing.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let hub = Arc::new(ClientHub::new());
    let err = StaticLicensePlugin::default()
        .init(&make_ctx(Arc::clone(&hub)))
        .await
        .expect_err("no admitted manifest");
    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub));
}

/// `init` binds the scoped client to the admitted instance.
#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let hub = Arc::new(ClientHub::new());
    let gear = StaticLicensePlugin::default();
    let manifest = gear
        .descriptor()
        .build_gts_manifest(&StaticConfigProvider { root: json!({}) })
        .expect("manifest");
    gear.init(
        &make_ctx(Arc::clone(&hub)).with_gts_publication_manifest_for_test(Arc::new(manifest)),
    )
    .await
    .expect("init with an admitted manifest");
    assert!(bound(&hub));
}

/// An invalid rule set is never advertised as a backend: it builds no
/// manifest. A rule whose resource type is a subject contract can never match.
#[test]
fn an_invalid_rule_set_publishes_nothing() {
    let config = StaticConfigProvider {
        root: json!({
            StaticLicensePlugin::MODULE_NAME: { "config": { "grants": [{
                "resource_type": crate::test_support::SUBJECT_TYPE,
                "subject_type": crate::test_support::SUBJECT_TYPE,
            }] } }
        }),
    };
    assert!(
        StaticLicensePlugin::default()
            .descriptor()
            .build_gts_manifest(&config)
            .is_err()
    );
}
