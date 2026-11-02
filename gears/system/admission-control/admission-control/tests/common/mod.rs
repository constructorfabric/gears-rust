//! Shared fixture of the gear-level integration tests: a config provider, a
//! platform registry fake and a context carrying the admitted manifest.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use admission_control::config::AdmissionControlConfig;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::gts_publication::GtsPublicationCapability as _;
use toolkit::{ClientHub, GearCtx};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

pub const GEAR: &str = "admission-control";
pub const WIDGET: &str = "gts.cf.core.test.widget.v1~";

pub const TENANT_SUBJECT: Uuid = Uuid::from_u128(0x1A);
pub const TENANT: Uuid = Uuid::from_u128(0x1B);

pub fn tenant_user() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(TENANT_SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// Gear config with every default and no engine.
pub fn config() -> AdmissionControlConfig {
    AdmissionControlConfig::default()
}

pub struct MockConfigProvider {
    gears: HashMap<String, Value>,
}

impl MockConfigProvider {
    pub fn new(config: &Value) -> Self {
        let mut gears = HashMap::new();
        gears.insert(GEAR.to_owned(), json!({ "config": config.clone() }));
        Self { gears }
    }
}

impl ConfigProvider for MockConfigProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.gears.get(gear_name)
    }
}

/// A context carrying the manifest admitted by the runtime before `init`.
pub fn gear_ctx(config: &AdmissionControlConfig) -> (GearCtx, Arc<ClientHub>) {
    gear_ctx_with(config, MockTypesRegistry::new())
}

pub fn gear_ctx_with(
    config: &AdmissionControlConfig,
    registry: MockTypesRegistry,
) -> (GearCtx, Arc<ClientHub>) {
    gear_ctx_raw(&serde_json::to_value(config).expect("config"), registry)
}

pub fn gear_ctx_raw(config: &Value, registry: MockTypesRegistry) -> (GearCtx, Arc<ClientHub>) {
    let hub = Arc::new(ClientHub::new());
    Arc::new(registry).install(&hub);
    let provider = Arc::new(MockConfigProvider::new(config));
    let manifest = admission_control::AdmissionControl::default()
        .descriptor()
        .build_gts_manifest(provider.as_ref())
        .expect("manifest");
    let ctx = GearCtx::new(
        GEAR,
        Uuid::new_v4(),
        provider,
        Arc::clone(&hub),
        CancellationToken::new(),
    )
    .with_gts_publication_manifest(Arc::new(manifest));
    (ctx, hub)
}
