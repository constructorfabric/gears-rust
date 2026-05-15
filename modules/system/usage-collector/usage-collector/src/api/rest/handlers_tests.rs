//! Unit tests for REST handlers and `domain_error_to_problem` / `canonical_error_to_problem`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use chrono::Utc;
use http::StatusCode;
use modkit::client_hub::{ClientHub, ClientScope};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::outbox_migrations;
use modkit_db::{ConnectOpts, connect_db};
use modkit_security::SecurityContext;
use types_registry_sdk::testing::make_test_instance;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, TypesRegistryError,
};
use usage_collector_sdk::{
    AllowedMetric, ModuleConfig, Subject, UsageCollectorClientV1, UsageCollectorError,
    UsageCollectorPluginClientV1, UsageCollectorPluginSpecV1, UsageKind, UsageRecord,
    UsageRecordError,
};
use usage_emitter::{UsageEmitterRuntime, UsageEmitterRuntimeV1};
use uuid::Uuid;

use super::canonical_error_to_problem;
use super::handle_create_usage_record;
use super::handle_get_module_config;
use crate::api::rest::dto::CreateUsageRecordRequest;
use crate::config::{MetricConfig, UsageCollectorConfig};
use crate::domain::Service;

// ── canonical_error_to_problem ──────────────────────────────────────

#[test]
fn canonical_internal_maps_to_500() {
    let err = UsageCollectorError::internal("something broke").create();
    let p = canonical_error_to_problem(&err);
    assert_eq!(p.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn canonical_not_found_maps_to_404() {
    let err = UsageRecordError::not_found("module not configured")
        .with_resource("test-module")
        .create();
    let p = canonical_error_to_problem(&err);
    assert_eq!(p.status, StatusCode::NOT_FOUND);
    assert_eq!(p.detail, "module not configured");
}

#[test]
fn canonical_service_unavailable_maps_to_503() {
    let err = UsageCollectorError::service_unavailable()
        .with_detail("transport error")
        .create();
    let p = canonical_error_to_problem(&err);
    assert_eq!(p.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(p.detail, "transport error");
}

#[test]
fn canonical_deadline_exceeded_maps_to_504() {
    let err = UsageRecordError::deadline_exceeded("plugin call timed out").create();
    let p = canonical_error_to_problem(&err);
    assert_eq!(p.status, StatusCode::GATEWAY_TIMEOUT);
}

#[test]
fn canonical_resource_exhausted_maps_to_429() {
    let err = UsageRecordError::resource_exhausted("query result too large")
        .with_quota_violation("rows", "row count exceeds limit")
        .create();
    let p = canonical_error_to_problem(&err);
    assert_eq!(p.status, StatusCode::TOO_MANY_REQUESTS);
}

// The canonical error's per-category ctx (reason / constraint / resource_*)
// must survive the Problem mapping as an RFC 9457 extension member, not get
// flattened into title/detail. Regression guard for MODKIT-ERR-001.
#[test]
fn canonical_error_to_problem_preserves_reason_extension() {
    let err = UsageRecordError::permission_denied()
        .with_reason("AUTHORIZATION_DENIED")
        .create();
    let p = canonical_error_to_problem(&err);

    assert_eq!(p.status, StatusCode::FORBIDDEN);
    let ctx = p.context.as_ref().expect("context must be populated");
    assert_eq!(
        ctx.get("reason").and_then(|v| v.as_str()),
        Some("AUTHORIZATION_DENIED"),
        "reason must be preserved as a Problem extension member: {ctx}"
    );
}

#[test]
fn canonical_error_to_problem_preserves_resource_extension() {
    let err = UsageRecordError::not_found("usage record not found")
        .with_resource("rec-1")
        .create();
    let p = canonical_error_to_problem(&err);

    assert_eq!(p.status, StatusCode::NOT_FOUND);
    let ctx = p.context.as_ref().expect("context must be populated");
    assert_eq!(
        ctx.get("resource_name").and_then(|v| v.as_str()),
        Some("rec-1"),
        "resource_name must be preserved as a Problem extension member: {ctx}"
    );
    assert_eq!(
        ctx.get("resource_type").and_then(|v| v.as_str()),
        Some("gts.cf.core.usage.record.v1~"),
        "resource_type must be preserved as a Problem extension member: {ctx}"
    );
}

// ── service builder helpers ─────────────────────────────────────────

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

fn service_with(
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
    metrics: HashMap<String, MetricConfig>,
) -> Arc<Service> {
    let instance_id = format!(
        "{}test.usage.mock.handlers_test.v1",
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

fn service_with_plugin(plugin: Arc<dyn UsageCollectorPluginClientV1>) -> Arc<Service> {
    service_with(plugin, HashMap::new())
}

struct OkPlugin;

#[async_trait]
impl UsageCollectorPluginClientV1 for OkPlugin {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }
}

// ── handle_get_module_config ──────────────────────────────────────

fn metrics_with(name: &str, kind: UsageKind) -> HashMap<String, MetricConfig> {
    let mut m = HashMap::new();
    m.insert(
        name.to_owned(),
        MetricConfig {
            kind,
            modules: None,
        },
    );
    m
}

#[tokio::test]
async fn get_module_config_handler_returns_allowed_metrics() {
    let svc = service_with(
        Arc::new(OkPlugin),
        metrics_with("cpu.usage", UsageKind::Gauge),
    );
    let result = handle_get_module_config(Path("my-module".to_owned()), Extension(svc)).await;

    let axum::Json(resp) = result.expect("handler should succeed");
    assert_eq!(resp.allowed_metrics.len(), 1);
    assert_eq!(resp.allowed_metrics[0].name, "cpu.usage");
    assert_eq!(resp.max_metadata_bytes, 8192);
}

#[tokio::test]
async fn get_module_config_handler_returns_404_for_unknown_module() {
    let svc = service_with_plugin(Arc::new(OkPlugin));
    let result = handle_get_module_config(Path("unknown-module".to_owned()), Extension(svc)).await;

    let err = result.expect_err("handler should return 404");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}

// ── handle_create_usage_record ────────────────────────────────────────────────

/// PDP mock that captures the `subject_id` and `subject_type` resource properties from the
/// incoming evaluation request, then allows the request to proceed.
struct CapturingSubjectAuthZ {
    captured_subject_id: Arc<Mutex<Option<String>>>,
    captured_subject_type: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl AuthZResolverClient for CapturingSubjectAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let subj_id = request
            .resource
            .properties
            .get("subject_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let subj_type = request
            .resource
            .properties
            .get("subject_type")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        *self.captured_subject_id.lock().unwrap() = subj_id;
        *self.captured_subject_type.lock().unwrap() = subj_type;
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Collector that returns a fixed `ModuleConfig` with one allowed metric.
struct FixedConfigCollector;

#[async_trait]
impl UsageCollectorClientV1 for FixedConfigCollector {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<ModuleConfig, UsageCollectorError> {
        Ok(ModuleConfig {
            allowed_metrics: vec![AllowedMetric {
                name: "test.gauge".to_owned(),
                kind: UsageKind::Gauge,
            }],
            max_metadata_bytes: 8192,
        })
    }
}

async fn build_handler_runtime(
    authz: Arc<dyn AuthZResolverClient>,
) -> Arc<dyn UsageEmitterRuntimeV1> {
    let db_name = format!("hw_{}", Uuid::new_v4().simple());
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
    let runtime = UsageEmitterRuntime::build(
        usage_emitter::UsageEmitterConfig::default(),
        db,
        authz,
        Arc::new(FixedConfigCollector),
    )
    .await
    .unwrap();
    Arc::new(runtime) as Arc<dyn UsageEmitterRuntimeV1>
}

#[tokio::test]
async fn ingest_handler_passes_subject_fields_to_authorize() {
    let subject_id = Uuid::new_v4();
    let subject_type = "test.service_account".to_owned();

    let captured_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_type: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let authz = Arc::new(CapturingSubjectAuthZ {
        captured_subject_id: Arc::clone(&captured_id),
        captured_subject_type: Arc::clone(&captured_type),
    });

    let runtime = build_handler_runtime(authz).await;

    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap();

    let req = CreateUsageRecordRequest {
        module: "test-module".to_owned(),
        tenant_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        resource_id: Uuid::new_v4(),
        subject: Some(Subject::with_type(subject_id, subject_type.clone())),
        metric: "test.gauge".to_owned(),
        idempotency_key: None,
        value: 1.0,
        timestamp: Utc::now(),
        metadata: None,
    };

    let result = handle_create_usage_record(Extension(ctx), Extension(runtime), Json(req)).await;

    assert!(result.is_ok(), "handler should succeed: {result:?}");

    assert_eq!(
        captured_id.lock().unwrap().as_deref(),
        Some(subject_id.to_string().as_str()),
        "subject_id must be forwarded to the PDP request"
    );
    assert_eq!(
        captured_type.lock().unwrap().as_deref(),
        Some(subject_type.as_str()),
        "subject_type must be forwarded to the PDP request"
    );
}

#[tokio::test]
async fn ingest_handler_succeeds_when_subject_fields_absent() {
    let captured_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_type: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let authz = Arc::new(CapturingSubjectAuthZ {
        captured_subject_id: Arc::clone(&captured_id),
        captured_subject_type: Arc::clone(&captured_type),
    });

    let runtime = build_handler_runtime(authz).await;

    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap();

    let req = CreateUsageRecordRequest {
        module: "test-module".to_owned(),
        tenant_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        resource_id: Uuid::new_v4(),
        subject: None,
        metric: "test.gauge".to_owned(),
        idempotency_key: None,
        value: 1.0,
        timestamp: Utc::now(),
        metadata: None,
    };

    let result = handle_create_usage_record(Extension(ctx), Extension(runtime), Json(req)).await;

    assert!(
        result.is_ok(),
        "handler should succeed when subject is absent: {result:?}"
    );

    // `subject: None` at the wire is the explicit "no subject" intent — the
    // gateway forwards it via `.without_subject()` on the factory. Per
    // ADR-0002 (forwarder substitution invariant), the gateway MUST NOT
    // substitute its own SecurityContext subject for the original caller,
    // so the PDP request carries neither SUBJECT_ID nor SUBJECT_TYPE.
    //
    // The resulting outbox payload is likewise subject-free: the emitter
    // handle binds `subject = None`, the prefilled builder skips its
    // `with_subject` step, and the SDK wire format omits the `subject`
    // field entirely (pinned by `models::tests::usage_record_subject_none_serde`
    // in usage-collector-sdk).
    assert!(
        captured_id.lock().unwrap().is_none(),
        "subject_id must NOT be forwarded to PDP when wire-level subject is absent (no gateway substitution)"
    );
    assert!(
        captured_type.lock().unwrap().is_none(),
        "subject_type must NOT be forwarded to PDP when wire-level subject is absent"
    );
}

#[tokio::test]
async fn ingest_handler_succeeds_when_subject_id_present_without_subject_type() {
    let subject_id = Uuid::new_v4();

    let captured_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_type: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let authz = Arc::new(CapturingSubjectAuthZ {
        captured_subject_id: Arc::clone(&captured_id),
        captured_subject_type: Arc::clone(&captured_type),
    });

    let runtime = build_handler_runtime(authz).await;

    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap();

    let req = CreateUsageRecordRequest {
        module: "test-module".to_owned(),
        tenant_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        resource_id: Uuid::new_v4(),
        subject: Some(Subject::new(subject_id)),
        metric: "test.gauge".to_owned(),
        idempotency_key: None,
        value: 1.0,
        timestamp: Utc::now(),
        metadata: None,
    };

    let result = handle_create_usage_record(Extension(ctx), Extension(runtime), Json(req)).await;

    assert!(
        result.is_ok(),
        "handler should succeed when subject_type is absent: {result:?}"
    );
    assert_eq!(
        captured_id.lock().unwrap().as_deref(),
        Some(subject_id.to_string().as_str()),
        "subject_id must be forwarded to PDP when subject_type is absent"
    );
    assert!(
        captured_type.lock().unwrap().is_none(),
        "subject_type must not be forwarded to PDP when absent"
    );
}
