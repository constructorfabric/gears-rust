#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, DenyReason};
use axum::routing::{get, post};
use axum::{Extension, Router};
use modkit::client_hub::{ClientHub, ClientScope};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::outbox_migrations;
use modkit_db::{ConnectOpts, connect_db};
use modkit_security::SecurityContext;
use modkit_security::access_scope::pep_properties;
use types_registry_sdk::testing::make_test_instance;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, TypesRegistryError,
};
use usage_collector::api::rest::handlers::{handle_create_usage_record, handle_get_module_config};
use usage_collector::config::{MetricConfig, UsageCollectorConfig};
use usage_collector::domain::Service;
use usage_collector_sdk::{
    AllowedMetric, ModuleConfig, UsageCollectorClientV1, UsageCollectorError,
    UsageCollectorPluginClientV1, UsageCollectorPluginSpecV1, UsageKind, UsageRecord,
};
use usage_emitter::{
    UsageEmitterConfig, UsageEmitterFactory, UsageEmitterRuntime, UsageEmitterRuntimeV1,
};
use uuid::Uuid;

// ── AuthZ mocks ───────────────────────────────────────────────────────────────

pub struct MockAuthZResolverClient {
    allow: bool,
    tenant_id: Uuid,
}

impl MockAuthZResolverClient {
    pub fn allow(tenant_id: Uuid) -> Self {
        Self {
            allow: true,
            tenant_id,
        }
    }

    #[allow(dead_code)]
    pub fn deny() -> Self {
        Self {
            allow: false,
            tenant_id: Uuid::nil(),
        }
    }
}

#[async_trait]
impl AuthZResolverClient for MockAuthZResolverClient {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        if self.allow {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [self.tenant_id],
                        ))],
                    }],
                    ..EvaluationResponseContext::default()
                },
            })
        } else {
            Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    deny_reason: Some(DenyReason {
                        error_code: "POLICY_DENIED".to_owned(),
                        details: None,
                    }),
                    ..EvaluationResponseContext::default()
                },
            })
        }
    }
}

// ── Collector mocks (used by the embedded emitter only) ───────────────────────

pub struct EmitterCollector {
    config: ModuleConfig,
}

impl EmitterCollector {
    fn new() -> Self {
        Self::with_metrics(vec![AllowedMetric {
            name: "test.gauge".to_owned(),
            kind: UsageKind::Gauge,
        }])
    }

    fn with_metrics(metrics: Vec<AllowedMetric>) -> Self {
        Self {
            config: ModuleConfig {
                allowed_metrics: metrics,
                max_metadata_bytes: 8192,
            },
        }
    }
}

#[async_trait]
impl UsageCollectorClientV1 for EmitterCollector {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }
    async fn get_module_config(&self, _: &str) -> Result<ModuleConfig, UsageCollectorError> {
        Ok(self.config.clone())
    }
}

// ── Plugin mock ───────────────────────────────────────────────────────────────

pub struct MockUsageCollectorPluginClientV1;

impl MockUsageCollectorPluginClientV1 {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl UsageCollectorPluginClientV1 for MockUsageCollectorPluginClientV1 {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }
}

// ── Service builder ───────────────────────────────────────────────────────────

struct StubRegistry {
    instances: Vec<GtsInstance>,
}

#[async_trait]
impl TypesRegistryClient for StubRegistry {
    async fn register(
        &self,
        _: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }
    async fn register_type_schemas(
        &self,
        _: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }
    async fn get_type_schema(&self, _: &str) -> Result<GtsTypeSchema, TypesRegistryError> {
        unimplemented!()
    }
    async fn get_type_schema_by_uuid(&self, _: Uuid) -> Result<GtsTypeSchema, TypesRegistryError> {
        unimplemented!()
    }
    async fn get_type_schemas(
        &self,
        _: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, TypesRegistryError>> {
        unimplemented!()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, TypesRegistryError>> {
        unimplemented!()
    }
    async fn list_type_schemas(
        &self,
        _: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, TypesRegistryError> {
        unimplemented!()
    }
    async fn register_instances(
        &self,
        _: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }
    async fn get_instance(&self, _: &str) -> Result<GtsInstance, TypesRegistryError> {
        unimplemented!()
    }
    async fn get_instance_by_uuid(&self, _: Uuid) -> Result<GtsInstance, TypesRegistryError> {
        unimplemented!()
    }
    async fn get_instances(
        &self,
        _: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, TypesRegistryError>> {
        unimplemented!()
    }
    async fn get_instances_by_uuid(
        &self,
        _: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, TypesRegistryError>> {
        unimplemented!()
    }
    async fn list_instances(
        &self,
        _: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, TypesRegistryError> {
        Ok(self.instances.clone())
    }
}

fn plugin_content(gts_id: &str, vendor: &str) -> serde_json::Value {
    serde_json::json!({
        "id": gts_id,
        "vendor": vendor,
        "priority": 0,
        "properties": {},
    })
}

#[allow(dead_code)]
pub fn build_service(
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
    metrics: HashMap<String, MetricConfig>,
) -> Arc<Service> {
    let instance_id = format!(
        "{}test.usage.mock.harness_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn TypesRegistryClient>(Arc::new(StubRegistry {
        instances: vec![make_test_instance(
            &instance_id,
            plugin_content(&instance_id, "cyberfabric"),
        )],
    }));
    hub.register_scoped::<dyn UsageCollectorPluginClientV1>(
        ClientScope::gts_id(&instance_id),
        plugin,
    );
    Arc::new(Service::new(
        UsageCollectorConfig {
            metrics,
            ..UsageCollectorConfig::default()
        },
        hub,
    ))
}

// ── Emitter mock ──────────────────────────────────────────────────────────────

/// Wraps a real [`UsageEmitterRuntime`] because [`usage_emitter::UsageEmitter::new`] is
/// `pub(crate)`. Implements [`UsageEmitterRuntimeV1`] by delegating `factory(...)` to the
/// wrapped runtime so callers exercise the real factory layer through the mock runtime.
pub struct MockUsageEmitterRuntimeV1(UsageEmitterRuntime);

impl MockUsageEmitterRuntimeV1 {
    pub async fn with_allow_authz() -> Self {
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        Self(build_real_runtime(authz, EmitterCollector::new()).await)
    }

    #[allow(dead_code)]
    pub async fn with_deny_authz() -> Self {
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolverClient::deny());
        Self(build_real_runtime(authz, EmitterCollector::new()).await)
    }

    #[allow(dead_code)]
    pub async fn with_allow_authz_and_metrics(metrics: Vec<AllowedMetric>) -> Self {
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        Self(build_real_runtime(authz, EmitterCollector::with_metrics(metrics)).await)
    }
}

impl UsageEmitterRuntimeV1 for MockUsageEmitterRuntimeV1 {
    fn factory(&self, module_name: &str) -> UsageEmitterFactory {
        self.0.factory(module_name)
    }
}

async fn build_real_runtime(
    authz: Arc<dyn AuthZResolverClient>,
    emitter_collector: EmitterCollector,
) -> UsageEmitterRuntime {
    let db_name = format!("uc_gw_{}", Uuid::new_v4().simple());
    let url = format!("sqlite:file:{db_name}?mode=memory&cache=shared");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .unwrap();
    let collector: Arc<dyn UsageCollectorClientV1> = Arc::new(emitter_collector);
    UsageEmitterRuntime::build(UsageEmitterConfig::default(), db, authz, collector)
        .await
        .unwrap()
}

// ── AppHarness ────────────────────────────────────────────────────────────────

pub struct AppHarness {
    pub router: Router,
}

impl AppHarness {
    #[allow(dead_code)]
    pub async fn new() -> Self {
        let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz().await)
            as Arc<dyn UsageEmitterRuntimeV1>;
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::new());
        let service = build_service(plugin, HashMap::new());
        Self::build(runtime, service, authz)
    }

    #[allow(dead_code)]
    pub fn with_emitter(runtime: Arc<dyn UsageEmitterRuntimeV1>) -> Self {
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::new());
        let service = build_service(plugin, HashMap::new());
        Self::build(runtime, service, authz)
    }

    #[allow(dead_code)]
    pub async fn with_metrics(metrics: HashMap<String, MetricConfig>) -> Self {
        let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz().await)
            as Arc<dyn UsageEmitterRuntimeV1>;
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::new());
        let service = build_service(plugin, metrics);
        Self::build(runtime, service, authz)
    }

    /// Build a harness whose emitter resolves the given allowed-metrics list when callers
    /// authorize a module. Use this to exercise counter / gauge validation rules end-to-end
    /// from the HTTP boundary.
    #[allow(dead_code)]
    pub async fn with_emitter_metrics(metrics: Vec<AllowedMetric>) -> Self {
        let runtime =
            Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz_and_metrics(metrics).await)
                as Arc<dyn UsageEmitterRuntimeV1>;
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::new());
        let service = build_service(plugin, HashMap::new());
        Self::build(runtime, service, authz)
    }

    fn build(
        runtime: Arc<dyn UsageEmitterRuntimeV1>,
        service: Arc<Service>,
        authz: Arc<dyn AuthZResolverClient>,
    ) -> Self {
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .unwrap();
        let router = Router::new()
            .route(
                "/usage-collector/v1/records",
                post(handle_create_usage_record),
            )
            .route(
                "/usage-collector/v1/modules/{module_name}/config",
                get(handle_get_module_config),
            )
            .layer(Extension(runtime))
            .layer(Extension(service))
            .layer(Extension(authz))
            .layer(Extension(ctx));
        Self { router }
    }
}
