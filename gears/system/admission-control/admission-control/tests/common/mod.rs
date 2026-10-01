//! Shared fixture of the gear-level integration tests: a config provider, a
//! types registry whose registration outcome is chosen per test, and a
//! `GearCtx` wired with them.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

pub const GEAR: &str = "admission-control";
pub const RESERVED_ID: &str = "reserved-name-prefix";
pub const WIDGET: &str = "gts.cf.core.test.widget.v1~";

/// A reserved-name-prefix built-in policy.
pub const RESERVED_NAME_PREFIX: &str = r#"
package builtin.reserved_name_prefix

reserved_prefixes := ["cf-", "system-"]

deny if {
    input.action in {"create", "rename"}
    some prefix in reserved_prefixes
    startswith(input.properties.name, prefix)
}
"#;

pub const TENANT_SUBJECT: Uuid = Uuid::from_u128(0x1A);
pub const TENANT: Uuid = Uuid::from_u128(0x1B);

pub fn tenant_user() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(TENANT_SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// Gear config with the reserved-name-prefix built-in and no engine.
pub fn config() -> Value {
    json!({
        "builtin_policies": [{
            "id": RESERVED_ID,
            "description": "Names with a reserved platform prefix cannot be created",
            "resource_types": [WIDGET],
            "actions": ["create", "rename"],
            "content": RESERVED_NAME_PREFIX,
        }]
    })
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

/// Outcome of the audit event-type registration.
#[derive(Clone, Copy)]
pub enum Registration {
    Accept,
    Reject,
    Unreachable,
}

/// The mock registry (which knows [`WIDGET`]) with a chosen registration
/// outcome; the mock itself panics on registration.
pub struct TestRegistry {
    inner: MockTypesRegistryClient,
    registration: Registration,
}

impl TestRegistry {
    pub fn new(registration: Registration) -> Self {
        Self {
            inner: MockTypesRegistryClient::new()
                .with_type_schemas([types_registry_sdk::testing::make_test_type_schema(WIDGET)]),
            registration,
        }
    }
}

#[async_trait]
impl TypesRegistryClient for TestRegistry {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        match self.registration {
            Registration::Unreachable => Err(CanonicalError::service_unavailable().create()),
            Registration::Accept => Ok(entities
                .iter()
                .map(|schema| RegisterResult::Ok {
                    gts_id: schema["$id"].as_str().unwrap_or_default().to_owned(),
                })
                .collect()),
            Registration::Reject => Ok(entities
                .iter()
                .map(|schema| RegisterResult::Err {
                    gts_id: schema["$id"].as_str().map(str::to_owned),
                    error: CanonicalError::internal("conflicting schema").create(),
                })
                .collect()),
        }
    }
    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_type_schemas(type_schemas).await
    }
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema(type_id).await
    }
    async fn get_type_schema_by_uuid(&self, id: Uuid) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema_by_uuid(id).await
    }
    async fn get_type_schemas(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas(ids).await
    }
    async fn get_type_schemas_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas_by_uuid(ids).await
    }
    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.inner.list_type_schemas(query).await
    }
    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_instances(instances).await
    }
    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance(id).await
    }
    async fn get_instance_by_uuid(&self, id: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance_by_uuid(id).await
    }
    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances(ids).await
    }
    async fn get_instances_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances_by_uuid(ids).await
    }
    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.inner.list_instances(query).await
    }
}

/// A `GearCtx` for the gear over a fresh hub holding the registry and the
/// decision point.
pub fn gear_ctx(config: &Value, registration: Registration) -> (GearCtx, Arc<ClientHub>) {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(TestRegistry::new(registration)));
    let ctx = GearCtx::new(
        GEAR,
        Uuid::new_v4(),
        Arc::new(MockConfigProvider::new(config)),
        Arc::clone(&hub),
        CancellationToken::new(),
    );
    (ctx, hub)
}
