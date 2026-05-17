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
use axum::extract::{Path, Query};
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use http::StatusCode;
use modkit::client_hub::{ClientHub, ClientScope};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::outbox_migrations;
use modkit_db::{ConnectOpts, connect_db};
use modkit_odata::SortDir;
use modkit_security::SecurityContext;
use types_registry_sdk::testing::make_test_instance;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, TypesRegistryError,
};
use usage_collector_sdk::models::{AggregationFn, BucketSize, GroupByDimension};
use usage_collector_sdk::{
    AggregationQuery, AggregationResult, AllowedMetric, CursorV1, ModuleConfig, Page, RawQuery,
    Subject, UsageCollectorClientV1, UsageCollectorError, UsageCollectorPluginClientV1,
    UsageCollectorPluginSpecV1, UsageKind, UsageRecord, UsageRecordError,
};
use usage_emitter::{UsageEmitterRuntime, UsageEmitterRuntimeV1};
use uuid::Uuid;

use super::canonical_error_to_problem;
use super::{MAX_FILTER_STRING_LEN, MAX_PAGE_SIZE, MAX_QUERY_TIME_RANGE};
use super::{handle_create_usage_record, handle_get_module_config};
use super::{handle_query_aggregated, handle_query_raw};
use crate::api::rest::dto::{
    AggregatedQueryParams, AggregationResultDto, CreateUsageRecordRequest, RawQueryParams,
};
use crate::config::{MetricConfig, UsageCollectorConfig};
use crate::domain::Service;
use crate::test_support::{
    DenyAuthZ, InternalErrorAuthZ, MultiConstraintAuthZ, NetworkErrorAuthZ, SingleConstraintAuthZ,
};

/// Extract `context.details` as a `Vec<String>` for 400 validation assertions.
///
/// The 400 envelope lives in `Problem.context` (RFC 9457 extension), not in
/// `Problem.detail`, so per-field error strings are read from `details[]`.
fn validation_details(problem: &modkit::api::problem::Problem) -> Vec<String> {
    problem
        .context
        .as_ref()
        .and_then(|c| c.get("details"))
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

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

/// Pin the wire-shape parity between the plugin-facing SDK type and the
/// gateway-side DTO. The `From<AggregationResult> for AggregationResultDto`
/// impl is field-for-field today; this test makes drift loud rather than
/// silent by round-tripping a fully-populated SDK value through JSON,
/// converting it field-by-field into the DTO, and asserting the DTO's JSON
/// is byte-identical to the SDK's JSON. Catches `#[serde(skip)]`, rename,
/// or `From` impl drift in one place — see the `# Why this is a structural
/// duplicate of AggregationResult` doc comment on `AggregationResultDto`.
#[test]
fn aggregation_result_and_dto_share_identical_json_wire_shape() {
    let bucket = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let sdk = AggregationResult {
        function: AggregationFn::Avg,
        value: 2.5,
        bucket_start: Some(bucket),
        usage_type: Some("network.bytes".to_owned()),
        subject_id: Some(Uuid::nil()),
        subject_type: Some("user".to_owned()),
        resource_id: Some(Uuid::nil()),
        resource_type: Some("compute.vm".to_owned()),
        source: Some("billing".to_owned()),
    };
    let dto = AggregationResultDto::from(sdk.clone());
    let sdk_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&sdk).unwrap()).unwrap();
    let dto_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&dto).unwrap()).unwrap();
    assert_eq!(
        sdk_json, dto_json,
        "AggregationResult and AggregationResultDto must serialize to identical JSON; \
         drift means the `From` impl, a serde rename, or `#[serde(skip)]` was changed \
         on one side without the other"
    );

    // Also pin that absent grouping dimensions (Option::None) are omitted on
    // both sides — a regression that flipped one side to emit `null` would
    // break wire-format parity without changing the field list.
    let sdk_sparse = AggregationResult {
        function: AggregationFn::Count,
        value: 42.0,
        bucket_start: None,
        usage_type: None,
        subject_id: None,
        subject_type: None,
        resource_id: None,
        resource_type: None,
        source: None,
    };
    let dto_sparse = AggregationResultDto::from(sdk_sparse.clone());
    let sdk_sparse_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&sdk_sparse).unwrap()).unwrap();
    let dto_sparse_json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&dto_sparse).unwrap()).unwrap();
    assert_eq!(sdk_sparse_json, dto_sparse_json);
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

/// PDP stub used by `service_with` for `Service::new`. The query API authz path is
/// not exercised by `handlers_tests` — these tests cover ingest + module-config only.
struct StubAlwaysAllowAuthZ;

#[async_trait]
impl AuthZResolverClient for StubAlwaysAllowAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
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
        Arc::new(StubAlwaysAllowAuthZ) as Arc<dyn AuthZResolverClient>,
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

    async fn query_aggregated(
        &self,
        _query: usage_collector_sdk::AggregationQuery,
    ) -> Result<Vec<usage_collector_sdk::AggregationResult>, UsageCollectorError> {
        Ok(vec![])
    }

    async fn query_raw(
        &self,
        _query: usage_collector_sdk::RawQuery,
    ) -> Result<usage_collector_sdk::Page<UsageRecord>, UsageCollectorError> {
        Ok(usage_collector_sdk::Page::empty(100))
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
            allowed_metrics: vec![
                AllowedMetric {
                    name: "test.gauge".to_owned(),
                    kind: UsageKind::Gauge,
                },
                AllowedMetric {
                    name: "test.counter".to_owned(),
                    kind: UsageKind::Counter,
                },
            ],
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

// `handle_create_usage_record` trims whitespace-only `idempotency_key` into
// `None` at the gateway boundary (handlers.rs:79-85). Pin both branches: a
// gauge collapses to `None` and the emitter auto-generates the UUID at enqueue
// time (success), while a counter collapses to `None` and the emitter rejects
// the record because counters require a caller-supplied key (canonical
// InvalidArgument → 400). A regression that drops the `.trim()` or
// `.filter(|k| !k.is_empty())` would forward the whitespace key verbatim and
// flip the gauge path to a 400 and the counter path to a 200 — both observable
// here.

#[tokio::test]
async fn ingest_handler_treats_whitespace_idempotency_key_as_none_for_gauge() {
    let runtime = build_handler_runtime(Arc::new(StubAlwaysAllowAuthZ)).await;

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
        idempotency_key: Some("   ".to_owned()),
        value: 1.0,
        timestamp: Utc::now(),
        metadata: None,
    };

    let result = handle_create_usage_record(Extension(ctx), Extension(runtime), Json(req)).await;

    assert!(
        result.is_ok(),
        "whitespace-only idempotency_key must collapse to None and let the gauge auto-generate a UUID; got: {result:?}"
    );
}

#[tokio::test]
async fn ingest_handler_treats_whitespace_idempotency_key_as_none_for_counter() {
    let runtime = build_handler_runtime(Arc::new(StubAlwaysAllowAuthZ)).await;

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
        metric: "test.counter".to_owned(),
        idempotency_key: Some("   ".to_owned()),
        value: 1.0,
        timestamp: Utc::now(),
        metadata: None,
    };

    let problem = handle_create_usage_record(Extension(ctx), Extension(runtime), Json(req))
        .await
        .expect_err(
            "whitespace-only idempotency_key on a counter must collapse to None and be rejected as InvalidArgument",
        );
    assert_eq!(
        problem.status,
        StatusCode::BAD_REQUEST,
        "canonical InvalidArgument must map to 400; got: {problem:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Query API tests — `handle_query_aggregated` / `handle_query_raw`
// ═════════════════════════════════════════════════════════════════════════
//
// These tests cover §6 acceptance criteria for the query endpoints:
// - happy path / 200 response shape
// - every validation branch (inst-agg-3/3a/3b/4, inst-raw-3/3a/3b/4)
// - PDP-denied / PDP-non-Denied (fail-closed) → 403 `{"error":"forbidden"}`
// - plugin Err → 503 `{"error":"service_unavailable","correlation_id":"..."}`
// - License-gate literal `gts.cf.core.lic.feat.v1~cf.core.global.base.v1`
// - OR-of-ANDs scope propagation (multi-constraint preserved, no flattening)
// - OpenAPI registry asserts `error_400` / `error_403` / `error_500` and the
//   503 `problem_response` are all declared so generated clients handle the
//   full set of statuses the handler can emit.
//
// `tenant_id` cannot be passed as a query parameter at the type level (the DTOs
// have no `tenant_id` field); we still pin this at runtime via a parse test on
// `serde_urlencoded` to guard against accidental field addition.
//
// HMAC / TTL cursors are NOT tested — `CursorV1` is opaque, keyset-only.
//
// Per CDSL Traceability Mode FULL, test bodies that validate a specific
// `inst-*` instruction wrap the asserting line(s) with `@cpt-begin`/`@cpt-end`
// markers.

// ── Query API helper types ────────────────────────────────────────────────
//
// The PDP mocks (`DenyAuthZ`, `NetworkErrorAuthZ`, `InternalErrorAuthZ`,
// `SingleConstraintAuthZ`, `MultiConstraintAuthZ`) live in
// `crate::test_support` so all in-crate unit tests share a single definition
// for the authz-correctness shapes.

/// Capturing plugin used to assert that the gateway forwards the PDP-compiled
/// `AccessScope` verbatim (OR-of-ANDs preserved) and to inspect the raw query
/// payload the gateway built (e.g. the decoded cursor). Returns empty result
/// sets from both query methods.
#[allow(clippy::struct_field_names)]
struct ScopeCapturingPlugin {
    captured_agg_constraint_groups: Arc<Mutex<Option<usize>>>,
    captured_raw_constraint_groups: Arc<Mutex<Option<usize>>>,
    captured_raw_cursor: Arc<Mutex<Option<CursorV1>>>,
    captured_raw_page_size: Arc<Mutex<Option<u32>>>,
}

impl ScopeCapturingPlugin {
    /// Constructor used when only the scope-propagation captures are needed;
    /// the raw cursor / `page_size` captures stay un-shared and inaccessible.
    fn scope_only(
        agg_groups: Arc<Mutex<Option<usize>>>,
        raw_groups: Arc<Mutex<Option<usize>>>,
    ) -> Self {
        Self {
            captured_agg_constraint_groups: agg_groups,
            captured_raw_constraint_groups: raw_groups,
            captured_raw_cursor: Arc::new(Mutex::new(None)),
            captured_raw_page_size: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl UsageCollectorPluginClientV1 for ScopeCapturingPlugin {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        *self.captured_agg_constraint_groups.lock().unwrap() =
            Some(query.scope.constraints().len());
        Ok(vec![])
    }

    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        *self.captured_raw_constraint_groups.lock().unwrap() =
            Some(query.scope.constraints().len());
        // `query.cursor` is `Option<CursorV1>`. Tests that exercise the
        // cursor-decode path always supply `Some(_)`, so storing it directly
        // (rather than wrapping in another `Option`) gives a flat
        // `Mutex<Option<CursorV1>>` whose `Some` exactly means "handler
        // forwarded the decoded cursor to the plugin".
        if let Some(cursor) = &query.cursor {
            *self.captured_raw_cursor.lock().unwrap() = Some(cursor.clone());
        }
        *self.captured_raw_page_size.lock().unwrap() = Some(query.page_size);
        Ok(Page::empty(u64::from(query.page_size)))
    }
}

/// Plugin that fails every query with a canonical `ServiceUnavailable` error.
/// Used to exercise the 503 mapping path (`inst-agg-8c` / `inst-raw-8b`).
struct UnavailablePlugin;

#[async_trait]
impl UsageCollectorPluginClientV1 for UnavailablePlugin {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn query_aggregated(
        &self,
        _query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        Err(UsageCollectorError::service_unavailable()
            .with_detail("plugin offline")
            .create())
    }

    async fn query_raw(&self, _query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        Err(UsageCollectorError::service_unavailable()
            .with_detail("plugin offline")
            .create())
    }
}

/// Plugin that fails every query with a canonical `ResourceExhausted` error.
/// Used to assert the gateway does NOT shortcut this to 400/429; it routes
/// through the same 503 canonical-Problem path as any other non-PermissionDenied
/// plugin error (planning decision: drop the 400 shortcut in favour of the
/// canonical mapper).
struct ResourceExhaustedPlugin;

#[async_trait]
impl UsageCollectorPluginClientV1 for ResourceExhaustedPlugin {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn query_aggregated(
        &self,
        _query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        Err(
            UsageRecordError::resource_exhausted("query result too large")
                .with_quota_violation("rows", "row count exceeds limit")
                .create(),
        )
    }

    async fn query_raw(&self, _query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        Err(
            UsageRecordError::resource_exhausted("query result too large")
                .with_quota_violation("rows", "row count exceeds limit")
                .create(),
        )
    }
}

/// Build a `Service` parameterised on a plugin and an authz mock so the same
/// helper can drive every query-handler test variant (happy / denied / 503 /
/// scope-propagation).
fn service_with_authz(
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
    authz: Arc<dyn AuthZResolverClient>,
) -> Arc<Service> {
    let instance_id = format!(
        "{}test.usage.mock.handlers_query_test.v1",
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
    Arc::new(Service::new(UsageCollectorConfig::default(), hub, authz))
}

/// `SecurityContext` stub. The `subject_id` doubles as the `correlation_id` the
/// handler emits in the 503 body and the ERROR log line, so tests capture it
/// for downstream assertions.
fn query_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("valid SecurityContext")
}

/// Convenience: build a permissive PDP stub that returns `decision=true` with
/// a single tenant constraint, satisfying `require_constraints(true)` so the
/// `access_scope_with` enforcer call resolves to `Ok(AccessScope)`. Use this in
/// tests that need to traverse the handler past the authz step.
fn allow_authz() -> Arc<dyn AuthZResolverClient> {
    Arc::new(SingleConstraintAuthZ {
        tenant_id: Uuid::new_v4(),
    })
}

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("valid RFC 3339")
        .with_timezone(&Utc)
}

/// Build a minimal valid `AggregatedQueryParams`. Tests mutate fields as needed.
fn agg_params_ok() -> AggregatedQueryParams {
    AggregatedQueryParams {
        fn_: AggregationFn::Sum,
        from: dt("2025-01-01T00:00:00Z"),
        to: dt("2025-01-02T00:00:00Z"),
        group_by: vec![],
        bucket_size: None,
        usage_type: None,
        subject_id: None,
        subject_type: None,
        resource_id: None,
        resource_type: None,
        source: None,
    }
}

/// Build a minimal valid `RawQueryParams`. Tests mutate fields as needed.
fn raw_params_ok() -> RawQueryParams {
    RawQueryParams {
        from: dt("2025-01-01T00:00:00Z"),
        to: dt("2025-01-02T00:00:00Z"),
        cursor: None,
        page_size: None,
        usage_type: None,
        subject_id: None,
        subject_type: None,
        resource_id: None,
        resource_type: None,
    }
}

/// Encode a synthetic `CursorV1` whose first key element is an RFC 3339 UTC
/// timestamp; used to exercise the `decode_and_validate_cursor` happy / bounds
/// branches.
fn encode_cursor_with_timestamp(ts: DateTime<Utc>) -> String {
    let cursor = CursorV1 {
        k: vec![ts.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    cursor.encode().expect("encode cursor")
}

/// Test shim for `decode_and_validate_cursor` that supplies the gateway's
/// canonical effective order (`+timestamp,+id`) and an empty effective filter
/// hash. Tests that focus on the cursor's structural validation (decode,
/// shape, sort direction, range) reach for this shim so they don't have to
/// hand-construct an `ODataOrderBy` per call site.
///
/// Sources the order from `usage_collector_sdk::raw_query_effective_order()`
/// rather than hand-rolling the `Vec<OrderKey>` so the test always agrees
/// byte-for-byte with the production call site — if the SDK changes the
/// effective order (e.g. adds a tiebreaker), every cursor-decode test below
/// re-aligns automatically instead of silently asserting against a stale
/// signature while the handler rejects every cursor.
fn decode_with_default_order(
    cursor_str: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    errors: &mut Vec<String>,
) -> Option<CursorV1> {
    super::decode_and_validate_cursor(
        cursor_str,
        from,
        to,
        usage_collector_sdk::raw_query_effective_order(),
        None,
        errors,
    )
}

// ── Aggregated handler tests ──────────────────────────────────────────────

#[tokio::test]
async fn handle_query_aggregated_returns_200_on_happy_path() {
    let captured: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
        Arc::clone(&captured),
        Arc::new(Mutex::new(None)),
    ));
    let svc = service_with_authz(plugin, allow_authz());
    let ctx = query_ctx();

    let result =
        handle_query_aggregated(Extension(ctx), Extension(svc), Query(agg_params_ok())).await;

    let Json(rows) = result.expect("happy path must return Ok(Json(_))");
    assert!(
        rows.is_empty(),
        "stub plugin returns no rows; response must be Vec<AggregationResultDto> shape"
    );
    // The plugin returns `Ok(vec![])`, so an empty response could come either
    // from delegation OR from a gateway short-circuit. Assert the capturing
    // plugin observed the call so this test actually exercises the
    // service → plugin path.
    assert!(
        captured.lock().unwrap().is_some(),
        "plugin must have been invoked -- empty result alone is not proof of delegation"
    );
}

#[tokio::test]
async fn handle_query_aggregated_400_when_from_ge_to() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = agg_params_ok();
    params.to = params.from; // from == to → still rejected (strictly ascending)

    let problem = handle_query_aggregated(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("from >= to must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("strictly ascending")),
        "context.details must mention strictly-ascending violation: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_aggregated_400_when_range_exceeds_max() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = agg_params_ok();
    // MAX_QUERY_TIME_RANGE is ~366 days; push `to` past that.
    params.to = params.from
        + ChronoDuration::from_std(MAX_QUERY_TIME_RANGE).expect("convertible")
        + ChronoDuration::seconds(1);

    let problem = handle_query_aggregated(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("range > MAX_QUERY_TIME_RANGE must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("maximum allowed duration")),
        "context.details must mention max-duration violation: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_aggregated_400_when_time_bucket_without_bucket_size() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = agg_params_ok();
    params.group_by = vec![GroupByDimension::TimeBucket(BucketSize::Hour)];
    params.bucket_size = None;

    let problem = handle_query_aggregated(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("time_bucket without bucket_size must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("bucket_size")),
        "context.details must mention bucket_size requirement: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_aggregated_400_when_filter_exceeds_max_len() {
    // Each filter string field has its own `push_filter_length_error` call
    // site in the handler. Iterate over every field so dropping any one of
    // those calls (a regression that would silently relax the length cap
    // for that field) is caught by this test.
    //
    // Field set MUST stay in sync with the `push_filter_length_error` calls
    // in `handle_query_aggregated` — if a new string filter is added there,
    // it MUST also appear in this list.
    for field in ["usage_type", "resource_type", "subject_type", "source"] {
        let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
        let mut params = agg_params_ok();
        let oversized = "a".repeat(MAX_FILTER_STRING_LEN + 1);
        match field {
            "usage_type" => params.usage_type = Some(oversized),
            "resource_type" => params.resource_type = Some(oversized),
            "subject_type" => params.subject_type = Some(oversized),
            "source" => params.source = Some(oversized),
            _ => unreachable!("field set is closed over the array literal above"),
        }

        let result =
            handle_query_aggregated(Extension(query_ctx()), Extension(svc), Query(params)).await;
        let Err(problem) = result else {
            panic!("oversized `{field}` must yield 400");
        };
        assert_eq!(
            problem.status,
            StatusCode::BAD_REQUEST,
            "oversized `{field}` must yield 400"
        );
        assert!(
            validation_details(&problem)
                .iter()
                .any(|d| d.contains(field) && d.contains("exceeds maximum")),
            "context.details must identify the oversized field `{field}`: {:?}",
            problem.context
        );
    }
}

#[tokio::test]
async fn handle_query_aggregated_collects_multiple_validation_errors_in_inst_agg_4a_body() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = agg_params_ok();
    params.to = params.from; // inst-agg-3a
    params.usage_type = Some("a".repeat(MAX_FILTER_STRING_LEN + 1)); // inst-agg-3 (filter len)

    let problem = handle_query_aggregated(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("multiple validation failures must yield a single 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4a
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4a
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4
    let body = problem
        .context
        .as_ref()
        .expect("400 must carry a structured validation envelope in `context`");
    assert_eq!(body["code"], "VALIDATION_ERROR");
    let details = body["details"].as_array().expect("details array");
    assert!(
        details.len() >= 2,
        "all detected violations must be accumulated into a single 400 body: {body}"
    );
}

#[tokio::test]
async fn handle_query_aggregated_403_when_pdp_denies() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(DenyAuthZ));
    let problem = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect_err("PDP-denied request must return 403");

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" })),
        "403 context must be the exact envelope -- no PDP details may leak"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_aggregated_403_when_pdp_network_error_fails_closed() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(NetworkErrorAuthZ));
    let problem = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect_err("PDP infra failure must fail-closed as 403, never 503");

    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" }))
    );
    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    // The authz layer must emit an ERROR log for any non-Denied PDP error.
    assert!(
        logs_contain("PDP infrastructure error"),
        "non-Denied PDP error must be captured at ERROR level"
    );
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_aggregated_403_when_pdp_internal_error_fails_closed() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(InternalErrorAuthZ));
    let problem = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect_err("PDP internal error must fail-closed as 403");

    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" }))
    );
    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    // `EnforcerError::CompileFailed` / `EvaluationFailed(Internal)` must take
    // the same ERROR-log branch as a transport failure — without this assertion,
    // a regression that silenced the log for the internal-error variants would
    // only be caught by the sibling network-error test.
    assert!(
        logs_contain("PDP infrastructure error"),
        "non-Denied PDP error (internal) must be captured at ERROR level"
    );
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_aggregated_503_on_plugin_error_emits_matching_correlation_id() {
    let svc = service_with_authz(Arc::new(UnavailablePlugin), allow_authz());

    let problem = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect_err("plugin error must surface as 503");

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
    assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = problem
        .context
        .as_ref()
        .expect("503 must carry the `error`/`correlation_id` envelope in `context`");
    assert_eq!(body["error"], "service_unavailable");
    // `correlation_id` is generated per-request (a fresh UUID v4), so we read
    // it from the response context and then assert the SAME id is present in
    // the captured ERROR log line. Pinning it to `ctx.subject_id()` would
    // collapse every 503 from the same caller into a single id.
    let correlation_id = body["correlation_id"]
        .as_str()
        .expect("503 body must carry a correlation_id");
    Uuid::parse_str(correlation_id)
        .expect("correlation_id must be a UUID -- fresh per request, never reused");
    assert!(
        logs_contain(correlation_id),
        "ERROR log must carry the same correlation_id emitted in the 503 body"
    );
    // Pin the structured-log triage schema. If a future refactor drops either
    // field, the high-signal partitioning channel (variant tag +
    // canonical status) becomes silently optional and every existing 503 test
    // continues to pass — this assertion makes that a deliberate break.
    // `UnavailablePlugin` returns `service_unavailable()` so the inner canonical
    // maps to status 503.
    assert!(
        logs_contain("error_variant=\"plugin\""),
        "ERROR log must carry the variant tag for triage; got logs without `error_variant`"
    );
    assert!(
        logs_contain("canonical_status_code=503"),
        "ERROR log must carry the inner canonical status code for triage"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
}

#[tokio::test]
async fn handle_query_aggregated_resource_exhausted_routes_through_canonical_503_not_400() {
    // Pre-resolved planning decision: the gateway no longer special-cases
    // ResourceExhausted into a 400/429 shortcut. The canonical Problem mapper
    // owns the status for any non-PermissionDenied plugin error; for the
    // gateway-side 503 envelope used by `service_unavailable_problem`, that
    // means ResourceExhausted MUST surface as 503 alongside every other
    // non-Denied DomainError::Plugin variant.
    let svc = service_with_authz(Arc::new(ResourceExhaustedPlugin), allow_authz());
    let problem = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect_err("ResourceExhausted must propagate as canonical 503");

    assert_eq!(
        problem.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "ResourceExhausted must NOT be turned into a 400 shortcut: {:?}",
        problem.context
    );
    let body = problem
        .context
        .as_ref()
        .expect("503 must carry the `error`/`correlation_id` envelope in `context`");
    assert_eq!(body["error"], "service_unavailable");
}

#[tokio::test]
async fn handle_query_aggregated_propagates_or_of_ands_scope() {
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let captured: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
        Arc::clone(&captured),
        Arc::new(Mutex::new(None)),
    ));
    let authz = Arc::new(MultiConstraintAuthZ { tenant_a, tenant_b });
    let svc = service_with_authz(plugin, authz);

    let _response: Json<_> = handle_query_aggregated(
        Extension(query_ctx()),
        Extension(svc),
        Query(agg_params_ok()),
    )
    .await
    .expect("multi-constraint PDP must allow the request");

    let groups = captured
        .lock()
        .unwrap()
        .expect("plugin must have been called");
    assert_eq!(
        groups, 2,
        "PDP OR-of-ANDs structure must be preserved when the handler forwards \
         the AccessScope to the plugin (flattening would widen the scope)"
    );
}

#[tokio::test]
async fn aggregated_query_params_dedupe_group_by_preserves_first_occurrence() {
    // Duplicate `?group_by=usage_type,usage_type` is normalized to a single
    // dimension before reaching the plugin. The plugin contract does not
    // promise idempotent group-by; deduping at the wire-level keeps an
    // out-of-tree backend from emitting duplicate columns or inflated rows.
    let qs = format!(
        "fn=sum&from={}&to={}&group_by=usage_type,usage_type",
        "2025-01-01T00:00:00Z", "2025-01-02T00:00:00Z",
    );
    let parsed: AggregatedQueryParams =
        serde_urlencoded::from_str(&qs).expect("valid aggregated query params");
    assert_eq!(
        parsed.group_by,
        vec![GroupByDimension::UsageType],
        "duplicate group_by dimensions must be deduped: got {:?}",
        parsed.group_by
    );
}

#[tokio::test]
async fn aggregated_query_params_dedupe_group_by_preserves_input_order() {
    let qs = format!(
        "fn=sum&from={}&to={}&group_by=resource,usage_type,resource,subject",
        "2025-01-01T00:00:00Z", "2025-01-02T00:00:00Z",
    );
    let parsed: AggregatedQueryParams =
        serde_urlencoded::from_str(&qs).expect("valid aggregated query params");
    assert_eq!(
        parsed.group_by,
        vec![
            GroupByDimension::Resource,
            GroupByDimension::UsageType,
            GroupByDimension::Subject,
        ],
        "dedupe must preserve the first-occurrence order: got {:?}",
        parsed.group_by
    );
}

#[tokio::test]
async fn aggregated_query_params_reject_tenant_id_field() {
    // §6 rule: the endpoint MUST NOT accept `tenant_id` as a query parameter.
    // With `#[serde(deny_unknown_fields)]` on the DTO, `serde_urlencoded` is
    // required to fail decode on any caller-supplied unknown field — in
    // particular `tenant_id` — so a future field addition cannot silently
    // re-open the hole.
    let qs = format!(
        "fn=sum&from={}&to={}&tenant_id={}",
        "2025-01-01T00:00:00Z",
        "2025-01-02T00:00:00Z",
        Uuid::new_v4()
    );
    let parsed: Result<AggregatedQueryParams, _> = serde_urlencoded::from_str(&qs);
    assert!(
        parsed.is_err(),
        "AggregatedQueryParams must reject unknown field `tenant_id`: got {parsed:?}"
    );
}

#[tokio::test]
async fn aggregated_query_params_reject_offset_aware_datetime() {
    // `inst-agg-3` requires callers to send UTC ('Z' or '+00:00'); offset-aware
    // values like `+05:00` MUST fail to decode at the deserialization boundary
    // so the gateway returns 400 before any handler logic runs. A regression
    // that loosened the offset check to "any RFC 3339" would let timezone-local
    // wall-clock times silently flow downstream.
    let qs_from = format!(
        "fn=sum&from={}&to={}",
        "2025-01-01T00:00:00+05:00", "2025-01-02T00:00:00Z"
    );
    let parsed_from: Result<AggregatedQueryParams, _> = serde_urlencoded::from_str(&qs_from);
    assert!(
        parsed_from.is_err(),
        "AggregatedQueryParams must reject offset-aware `from`: got {parsed_from:?}"
    );

    let qs_to = format!(
        "fn=sum&from={}&to={}",
        "2025-01-01T00:00:00Z", "2025-01-02T00:00:00-08:00"
    );
    let parsed_to: Result<AggregatedQueryParams, _> = serde_urlencoded::from_str(&qs_to);
    assert!(
        parsed_to.is_err(),
        "AggregatedQueryParams must reject offset-aware `to`: got {parsed_to:?}"
    );
}

// ── Raw handler tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn handle_query_raw_returns_200_with_empty_page_on_happy_path() {
    let captured: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
        Arc::new(Mutex::new(None)),
        Arc::clone(&captured),
    ));
    let svc = service_with_authz(plugin, allow_authz());

    let result = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await;
    let Json(page) = result.expect("happy path must return Ok(Json(Page))");
    assert!(page.items.is_empty(), "noop plugin returns no items");
    assert!(
        page.page_info.next_cursor.is_none(),
        "empty page must omit next_cursor -- final page marker per section 6"
    );
    // An empty page is consistent with a gateway short-circuit that never
    // reached the plugin; assert the capturing plugin actually saw the call.
    assert!(
        captured.lock().unwrap().is_some(),
        "plugin must have been invoked -- empty page alone is not proof of delegation"
    );
}

#[tokio::test]
async fn handle_query_raw_400_when_from_ge_to() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    params.to = params.from;

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("from >= to must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a
}

#[tokio::test]
async fn handle_query_raw_400_when_range_exceeds_max() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    params.to = params.from
        + ChronoDuration::from_std(MAX_QUERY_TIME_RANGE).expect("convertible")
        + ChronoDuration::seconds(1);

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("range > MAX_QUERY_TIME_RANGE must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
}

#[tokio::test]
async fn handle_query_raw_400_on_page_size_zero() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    params.page_size = Some(0);

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("page_size=0 must yield 400");
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("page_size")),
        "context.details must mention page_size: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_raw_400_on_page_size_above_max() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    params.page_size = Some(MAX_PAGE_SIZE + 1);

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("page_size > MAX_PAGE_SIZE must yield 400");
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("page_size")),
        "context.details must mention page_size: {:?}",
        problem.context
    );
}

// Default-`page_size`-substitution coverage lives in
// `tests/query_raw_tests.rs::query_raw_absent_page_size_uses_default_page_size`,
// which drives the gateway end-to-end through `MockUsageCollectorPluginClientV1`
// and asserts `observed_page_size == Some(DEFAULT_PAGE_SIZE)`. The previous
// hermetic-only `is_ok()` assertion here was coverage theater — it passed
// regardless of whether the gateway forwarded `100`, `1`, or `999_999_999` —
// so it has been removed in favour of the integration test.

#[tokio::test]
async fn handle_query_raw_400_on_malformed_cursor() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    params.cursor = Some("not-a-valid-cursor-string!!!".to_owned());

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("malformed cursor must yield 400");
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("cursor")),
        "context.details must mention cursor: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_raw_400_on_cursor_outside_time_range() {
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let mut params = raw_params_ok();
    // Cursor timestamp before `from` → must be rejected.
    let outside_ts = params.from - ChronoDuration::days(7);
    params.cursor = Some(encode_cursor_with_timestamp(outside_ts));

    let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
        .await
        .expect_err("cursor outside [from, to] must yield 400");
    assert_eq!(problem.status, StatusCode::BAD_REQUEST);
    assert!(
        validation_details(&problem)
            .iter()
            .any(|d| d.contains("cursor") && d.contains("[from, to]")),
        "context.details must mention cursor range violation: {:?}",
        problem.context
    );
}

#[tokio::test]
async fn handle_query_raw_accepts_cursor_inside_time_range() {
    // CursorV1 round-trip — no HMAC, no TTL — just keyset traversal.
    //
    // Capture the `RawQuery` the handler forwarded so we can assert the
    // decoded cursor actually reached `query.cursor`. Without this, a
    // regression that drops the cursor (e.g. `query.cursor = None`) would
    // not fail the test — `OkPlugin` discards its input unconditionally.
    let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(None)),
    ));
    let captured_cursor = Arc::clone(&plugin.captured_raw_cursor);
    let svc = service_with_authz(plugin, allow_authz());
    let mut params = raw_params_ok();
    let inside_ts = params.from + ChronoDuration::hours(1);
    params.cursor = Some(encode_cursor_with_timestamp(inside_ts));

    let result = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params)).await;
    let Json(page) = result.expect("in-range cursor must be accepted");
    assert!(page.items.is_empty(), "stub plugin returns empty page");
    assert!(
        page.page_info.next_cursor.is_none(),
        "final page must have absent next_cursor -- valid success per section 6"
    );

    let observed = captured_cursor
        .lock()
        .unwrap()
        .clone()
        .expect("handler must forward the decoded cursor to the plugin");
    // The decoded cursor's first key element is the RFC 3339 timestamp the
    // test seeded above; the second is a UUID. Both must round-trip verbatim.
    assert_eq!(
        observed.k.len(),
        2,
        "decoded cursor key must carry the (timestamp, id) tuple seeded by the test"
    );
    let observed_ts = DateTime::parse_from_rfc3339(observed.k[0].as_str())
        .expect("cursor timestamp must remain RFC 3339")
        .with_timezone(&Utc);
    assert_eq!(
        observed_ts, inside_ts,
        "handler must forward the cursor's decoded timestamp verbatim to the plugin"
    );
}

/// Filter-field selector used by
/// [`handle_query_raw_filter_hash_gate_is_wired_end_to_end`]. One variant per
/// mutable field so a future filter addition is a compile-time forcing
/// function on the test matrix rather than a silent gap.
enum FilterMutation {
    UsageType,
    ResourceType,
    SubjectType,
    ResourceId,
    SubjectId,
}

#[tokio::test]
async fn handle_query_raw_filter_hash_gate_is_wired_end_to_end() {
    // `decode_and_validate_cursor` and `raw_query_filter_hash` are both
    // unit-tested in isolation; this is the end-to-end pin that the handler
    // actually feeds the request's filter set into the hash and threads the
    // resulting digest into `decode_and_validate_cursor`. The named-field
    // `RawQueryFilters` struct exists specifically to make a `resource_type`
    // ↔ `subject_type` swap (or an omitted field) at the handler call site
    // visible as a hash disagreement — but the protection only fires across
    // the wire, so a unit test of the SDK helper alone cannot exercise it.
    //
    // Drive the matrix from one base request: mint a cursor whose `f`
    // matches the full-filter request, replay it once with the same filters
    // (must succeed, must reach the plugin), and once per field with that
    // single field altered (each must yield 400 with the
    // `CURSOR_ERR_FILTER_MISMATCH` constant). A regression that drops, swaps,
    // or aliases any of the five fields at the handler call site flips at
    // least one of these branches.
    let from = dt("2025-01-01T00:00:00Z");
    let to = from + ChronoDuration::hours(1);
    let inside = from + ChronoDuration::minutes(15);

    let usage_type = "compute".to_owned();
    let resource_type = "vm".to_owned();
    let subject_type = "user".to_owned();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();

    let filters = usage_collector_sdk::RawQueryFilters {
        from,
        to,
        usage_type: Some(usage_type.as_str()),
        resource_id: Some(resource_id),
        resource_type: Some(resource_type.as_str()),
        subject_type: Some(subject_type.as_str()),
        subject_id: Some(subject_id),
    };
    let effective_hash = usage_collector_sdk::raw_query_filter_hash(&filters);

    let cursor = CursorV1 {
        k: vec![inside.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: Some(effective_hash.clone()),
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");

    let base_params = || RawQueryParams {
        from,
        to,
        cursor: Some(token.clone()),
        page_size: None,
        usage_type: Some(usage_type.clone()),
        subject_id: Some(subject_id),
        subject_type: Some(subject_type.clone()),
        resource_id: Some(resource_id),
        resource_type: Some(resource_type.clone()),
    };

    // (1) Replay with matching filters — must succeed and reach the plugin.
    {
        let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
        ));
        let captured_cursor = Arc::clone(&plugin.captured_raw_cursor);
        let svc = service_with_authz(plugin, allow_authz());
        let result =
            handle_query_raw(Extension(query_ctx()), Extension(svc), Query(base_params())).await;
        let Json(_) = result.expect("matching filter hash must be accepted");
        assert!(
            captured_cursor.lock().unwrap().is_some(),
            "handler must forward the decoded cursor to the plugin on the matching-hash branch",
        );
    }

    // (2) Replay each per-field alteration — each must yield 400 with
    //     `CURSOR_ERR_FILTER_MISMATCH`. Listing the field set explicitly
    //     means a future filter addition forces a new case rather than
    //     silently widening the existing ones.
    let mutations = [
        ("usage_type", FilterMutation::UsageType),
        ("resource_type", FilterMutation::ResourceType),
        ("subject_type", FilterMutation::SubjectType),
        ("resource_id", FilterMutation::ResourceId),
        ("subject_id", FilterMutation::SubjectId),
    ];

    for (label, mutation) in mutations {
        let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
        let mut params = base_params();
        match mutation {
            FilterMutation::UsageType => params.usage_type = Some("storage".to_owned()),
            FilterMutation::ResourceType => params.resource_type = Some("container".to_owned()),
            FilterMutation::SubjectType => params.subject_type = Some("service".to_owned()),
            FilterMutation::ResourceId => params.resource_id = Some(Uuid::new_v4()),
            FilterMutation::SubjectId => params.subject_id = Some(Uuid::new_v4()),
        }

        let problem = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params))
            .await
            .expect_err(&format!("altering `{label}` must produce a 400, got Ok"));

        assert_eq!(
            problem.status,
            StatusCode::BAD_REQUEST,
            "altering `{label}` must yield 400; got {:?}",
            problem.status,
        );
        assert!(
            validation_details(&problem)
                .iter()
                .any(|d| d == super::CURSOR_ERR_FILTER_MISMATCH),
            "altering `{label}` must surface CURSOR_ERR_FILTER_MISMATCH; details = {:?}",
            problem.context,
        );
    }
}

// Pin each of `decode_and_validate_cursor`'s failure modes against the
// `CURSOR_ERR_*` named constants in `handlers.rs`. The strings end up in the
// 400 validation envelope (externally visible), so a copy-edit must be a
// deliberate wire-contract change — but routing through the constants means a
// wording tweak is a one-line edit instead of a co-ordinated rename across
// every test below.

#[test]
fn decode_and_validate_cursor_pins_decode_failed_string() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let mut errors = Vec::new();
    let out = decode_with_default_order("not-a-valid-cursor!!!", from, to, &mut errors);
    assert!(out.is_none());
    assert_eq!(errors, vec![super::CURSOR_ERR_DECODE_FAILED.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_pins_invalid_key_shape_string() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let cursor = CursorV1 {
        k: vec!["only-one-element".to_owned()],
        o: SortDir::Asc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none());
    assert_eq!(errors, vec![super::CURSOR_ERR_INVALID_KEY_SHAPE.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_pins_invalid_timestamp_string() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let cursor = CursorV1 {
        k: vec![
            "not-an-rfc3339-timestamp".to_owned(),
            Uuid::new_v4().to_string(),
        ],
        o: SortDir::Asc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none());
    assert_eq!(errors, vec![super::CURSOR_ERR_INVALID_TIMESTAMP.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_rejects_descending_sort_direction() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let cursor = CursorV1 {
        k: vec![from.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Desc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none(), "Desc cursor must be rejected");
    assert!(
        errors
            .iter()
            .any(|e| e == super::CURSOR_ERR_UNSUPPORTED_SORT),
        "must surface `CURSOR_ERR_UNSUPPORTED_SORT`; got: {errors:?}"
    );
}

#[test]
fn decode_and_validate_cursor_rejects_backward_pagination_direction() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let cursor = CursorV1 {
        k: vec![from.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "bwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none(), "bwd cursor must be rejected");
    assert!(
        errors
            .iter()
            .any(|e| e == super::CURSOR_ERR_UNSUPPORTED_PAGINATION),
        "must surface `CURSOR_ERR_UNSUPPORTED_PAGINATION`; got: {errors:?}"
    );
}

#[test]
fn decode_and_validate_cursor_rejects_non_uuid_id_at_k1() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let cursor = CursorV1 {
        k: vec![from.to_rfc3339(), "not-a-uuid".to_owned()],
        o: SortDir::Asc,
        s: "timestamp,id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none(), "non-UUID id must be rejected");
    assert!(
        errors.iter().any(|e| e == super::CURSOR_ERR_INVALID_ID),
        "must surface `CURSOR_ERR_INVALID_ID`; got: {errors:?}"
    );
}

#[test]
fn decode_and_validate_cursor_pins_outside_range_string() {
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let outside_ts = from - ChronoDuration::days(7);
    let token = encode_cursor_with_timestamp(outside_ts);
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none());
    assert_eq!(errors, vec![super::CURSOR_ERR_OUTSIDE_RANGE.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_rejects_mismatched_sort_signature() {
    // A cursor with `s: "+id,+timestamp"` (or any signature other than
    // `+timestamp,+id`) was minted under a different sort order; the gateway
    // must reject it before forwarding to the plugin, otherwise the storage
    // backend has no way to detect that the cursor's keyset does not match
    // the order it is paginating.
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let inside = from + ChronoDuration::minutes(15);
    let cursor = CursorV1 {
        k: vec![inside.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+id,+timestamp".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = decode_with_default_order(&token, from, to, &mut errors);
    assert!(out.is_none(), "mismatched sort signature must be rejected");
    assert_eq!(errors, vec![super::CURSOR_ERR_ORDER_MISMATCH.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_rejects_mismatched_filter_hash() {
    // A cursor stamped with a `cursor.f` that does not match the request's
    // effective filter hash must be rejected — this catches the case where a
    // caller pastes a cursor minted under a different filter set into a new
    // request with different filters.
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let inside = from + ChronoDuration::minutes(15);
    let cursor = CursorV1 {
        k: vec![inside.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: Some("deadbeefcafebabe".to_owned()),
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = super::decode_and_validate_cursor(
        &token,
        from,
        to,
        usage_collector_sdk::raw_query_effective_order(),
        Some("0123456789abcdef"),
        &mut errors,
    );
    assert!(out.is_none(), "mismatched filter hash must be rejected");
    assert_eq!(errors, vec![super::CURSOR_ERR_FILTER_MISMATCH.to_owned()]);
}

#[test]
fn decode_and_validate_cursor_accepts_cursor_with_no_filter_hash() {
    // `validate_cursor_against` is "if both Some, must agree" — a cursor
    // minted with `f = None` must continue to pass the consistency check
    // even when the gateway computes a hash for the current request. This
    // preserves backward compatibility with any storage plugin that does
    // not yet mint `cursor.f`.
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let inside = from + ChronoDuration::minutes(15);
    let cursor = CursorV1 {
        k: vec![inside.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = super::decode_and_validate_cursor(
        &token,
        from,
        to,
        usage_collector_sdk::raw_query_effective_order(),
        Some("0123456789abcdef"),
        &mut errors,
    );
    assert!(
        out.is_some(),
        "cursor with f = None must be accepted regardless of effective hash"
    );
    assert!(errors.is_empty(), "no errors expected; got {errors:?}");
}

#[test]
fn decode_and_validate_cursor_accepts_cursor_with_matching_filter_hash() {
    // The "both Some, must agree" branch of `validate_cursor_against` has
    // negative-path coverage (mismatched hash → rejected) but no positive
    // case in the existing tests. Without a green case, an inversion of
    // the agreement logic — or a drift in the canonical-string format
    // that accidentally makes the negative test's bogus hash match the
    // real one — would not be caught by the negative tests alone.
    //
    // Construct a cursor whose `f` equals the real hash for a specific
    // filter set, pass it through `decode_and_validate_cursor` with the
    // same effective hash, and assert it is accepted with no errors.
    let from = Utc::now();
    let to = from + ChronoDuration::hours(1);
    let inside = from + ChronoDuration::minutes(15);
    let filters = usage_collector_sdk::RawQueryFilters {
        from,
        to,
        usage_type: Some("compute"),
        resource_id: Some(Uuid::new_v4()),
        resource_type: Some("vm"),
        subject_type: None,
        subject_id: None,
    };
    let effective_hash = usage_collector_sdk::raw_query_filter_hash(&filters);
    let cursor = CursorV1 {
        k: vec![inside.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: Some(effective_hash.clone()),
        d: "fwd".to_owned(),
    };
    let token = cursor.encode().expect("encode cursor");
    let mut errors = Vec::new();
    let out = super::decode_and_validate_cursor(
        &token,
        from,
        to,
        usage_collector_sdk::raw_query_effective_order(),
        Some(effective_hash.as_str()),
        &mut errors,
    );
    assert!(
        out.is_some(),
        "cursor with matching s and f must be accepted; errors = {errors:?}"
    );
    assert!(errors.is_empty(), "no errors expected; got {errors:?}");
}

#[tokio::test]
async fn handle_query_raw_400_when_filter_exceeds_max_len() {
    // Each filter string field has its own `push_filter_length_error` call
    // site in the raw handler. Iterate over every field so dropping any one
    // of those calls is caught by this test.
    //
    // Field set MUST stay in sync with the `push_filter_length_error` calls
    // in `handle_query_raw` — `RawQueryParams` has no `source` field, unlike
    // the aggregated DTO.
    for field in ["usage_type", "resource_type", "subject_type"] {
        let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
        let mut params = raw_params_ok();
        let oversized = "a".repeat(MAX_FILTER_STRING_LEN + 1);
        match field {
            "usage_type" => params.usage_type = Some(oversized),
            "resource_type" => params.resource_type = Some(oversized),
            "subject_type" => params.subject_type = Some(oversized),
            _ => unreachable!("field set is closed over the array literal above"),
        }

        let result = handle_query_raw(Extension(query_ctx()), Extension(svc), Query(params)).await;
        let Err(problem) = result else {
            panic!("oversized `{field}` must yield 400");
        };
        assert_eq!(
            problem.status,
            StatusCode::BAD_REQUEST,
            "oversized `{field}` must yield 400"
        );
        assert!(
            validation_details(&problem)
                .iter()
                .any(|d| d.contains(field) && d.contains("exceeds maximum")),
            "context.details must identify the oversized field `{field}`: {:?}",
            problem.context
        );
    }
}

#[tokio::test]
async fn handle_query_raw_403_when_pdp_denies() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(DenyAuthZ));
    let problem = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await
    .expect_err("PDP-denied request must return 403");

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" })),
        "403 context must be the exact envelope -- no PDP details may leak"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_raw_403_when_pdp_network_error_fails_closed() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(NetworkErrorAuthZ));
    let problem = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await
    .expect_err("PDP infra failure must fail-closed as 403");

    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" }))
    );
    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    // Mirror of the aggregated network-error test: any non-Denied PDP error
    // MUST surface at ERROR level so operators can triage authz infra
    // failures separately from genuine deny decisions.
    assert!(
        logs_contain("PDP infrastructure error"),
        "non-Denied PDP error (network) must be captured at ERROR level"
    );
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_raw_403_when_pdp_internal_error_fails_closed() {
    let svc = service_with_authz(Arc::new(OkPlugin), Arc::new(InternalErrorAuthZ));
    let problem = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await
    .expect_err("PDP internal error must fail-closed as 403");

    assert_eq!(problem.status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.context,
        Some(serde_json::json!({ "error": "forbidden" }))
    );
    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    // Parity with the aggregated internal-error test — `CompileFailed` /
    // `EvaluationFailed(Internal)` must take the ERROR-log branch on the raw
    // path too.
    assert!(
        logs_contain("PDP infrastructure error"),
        "non-Denied PDP error (internal) must be captured at ERROR level"
    );
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
}

#[tokio::test]
#[tracing_test::traced_test]
async fn handle_query_raw_503_on_plugin_error_emits_matching_correlation_id() {
    let svc = service_with_authz(Arc::new(UnavailablePlugin), allow_authz());

    let problem = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await
    .expect_err("plugin error must surface as 503");

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
    assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = problem
        .context
        .as_ref()
        .expect("503 must carry the `error`/`correlation_id` envelope in `context`");
    assert_eq!(body["error"], "service_unavailable");
    let correlation_id = body["correlation_id"]
        .as_str()
        .expect("503 body must carry a correlation_id");
    Uuid::parse_str(correlation_id)
        .expect("correlation_id must be a UUID -- fresh per request, never reused");
    assert!(
        logs_contain(correlation_id),
        "ERROR log must carry the same correlation_id emitted in the 503 body"
    );
    // Parity with the aggregated 503 test: pin the structured-log triage
    // schema so dropping `error_variant` / `canonical_status_code` is caught
    // here rather than silently in production observability storage.
    assert!(
        logs_contain("error_variant=\"plugin\""),
        "ERROR log must carry the variant tag for triage"
    );
    assert!(
        logs_contain("canonical_status_code=503"),
        "ERROR log must carry the inner canonical status code for triage"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
}

#[tokio::test]
async fn handle_query_raw_propagates_or_of_ands_scope() {
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let captured: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let plugin = Arc::new(ScopeCapturingPlugin::scope_only(
        Arc::new(Mutex::new(None)),
        Arc::clone(&captured),
    ));
    let svc = service_with_authz(
        plugin,
        Arc::new(MultiConstraintAuthZ { tenant_a, tenant_b }),
    );

    let _response: Json<_> = handle_query_raw(
        Extension(query_ctx()),
        Extension(svc),
        Query(raw_params_ok()),
    )
    .await
    .expect("multi-constraint PDP must allow the request");

    let groups = captured
        .lock()
        .unwrap()
        .expect("plugin must have been called");
    assert_eq!(
        groups, 2,
        "raw handler must preserve OR-of-ANDs when forwarding AccessScope"
    );
}

#[tokio::test]
async fn raw_query_params_reject_tenant_id_field() {
    // Mirror of `aggregated_query_params_reject_tenant_id_field`: the DTO is
    // marked `#[serde(deny_unknown_fields)]`, so a caller-supplied `tenant_id`
    // MUST fail to decode rather than be silently dropped.
    let qs = format!(
        "from={}&to={}&tenant_id={}",
        "2025-01-01T00:00:00Z",
        "2025-01-02T00:00:00Z",
        Uuid::new_v4()
    );
    let parsed: Result<RawQueryParams, _> = serde_urlencoded::from_str(&qs);
    assert!(
        parsed.is_err(),
        "RawQueryParams must reject unknown field `tenant_id`: got {parsed:?}"
    );
}

#[tokio::test]
async fn raw_query_params_reject_offset_aware_datetime() {
    // Mirror of `aggregated_query_params_reject_offset_aware_datetime`:
    // `inst-raw-3` requires UTC datetimes; offset-aware values MUST fail to
    // decode at the deserialization boundary so the gateway returns 400 before
    // any handler logic runs.
    let qs_from = format!(
        "from={}&to={}",
        "2025-01-01T00:00:00+05:00", "2025-01-02T00:00:00Z"
    );
    let parsed_from: Result<RawQueryParams, _> = serde_urlencoded::from_str(&qs_from);
    assert!(
        parsed_from.is_err(),
        "RawQueryParams must reject offset-aware `from`: got {parsed_from:?}"
    );

    let qs_to = format!(
        "from={}&to={}",
        "2025-01-01T00:00:00Z", "2025-01-02T00:00:00-08:00"
    );
    let parsed_to: Result<RawQueryParams, _> = serde_urlencoded::from_str(&qs_to);
    assert!(
        parsed_to.is_err(),
        "RawQueryParams must reject offset-aware `to`: got {parsed_to:?}"
    );
}

// ── License gate ──────────────────────────────────────────────────────────

#[test]
fn license_gate_literal_matches_feature_0003_dod() {
    // Per FEATURE 0003 DoD §5: both query endpoints MUST be gated on the
    // platform license feature `gts.cf.core.lic.feat.v1~cf.core.global.base.v1`,
    // and the gate returns 403 BEFORE any PDP call. This test pins the
    // literal so a typo cannot silently disable the gate; the constant lives
    // in `routes.rs` and is the single source of truth used by the `License`
    // marker the route registration consumes. Wiring of the marker against
    // each route is asserted separately by
    // `license_gate_wired_for_both_query_routes` below.
    assert_eq!(
        crate::api::rest::routes::LICENSE_FEATURE,
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1",
        "license gate literal must match FEATURE 0003 DoD section 5 exactly"
    );
}

#[tokio::test]
async fn license_gate_wired_for_both_query_routes() {
    // Pinning the license literal alone does not catch a typo at the
    // `OperationBuilder::require_license_features::<License>(...)` call site:
    // a route could quietly drop the gate and the literal-only test would
    // still pass. Introspect the registered operation specs and assert the
    // license feature is wired against BOTH query routes.
    use modkit::api::OpenApiRegistryImpl;

    let registry = OpenApiRegistryImpl::new();
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    let runtime = build_handler_runtime(Arc::new(StubAlwaysAllowAuthZ)).await;
    let _router =
        crate::api::rest::routes::register_routes(axum::Router::new(), &registry, runtime, svc);

    let expected = crate::api::rest::routes::LICENSE_FEATURE;
    for path in [
        "GET:/usage-collector/v1/aggregated",
        "GET:/usage-collector/v1/raw",
    ] {
        let spec = registry
            .operation_specs
            .get(path)
            .unwrap_or_else(|| panic!("operation spec missing for {path}"));
        let req = spec
            .license_requirement
            .as_ref()
            .unwrap_or_else(|| panic!("{path} must declare a license requirement"));
        assert!(
            req.license_names.iter().any(|f| f == expected),
            "{path} must require license feature {expected}; got {:?}",
            req.license_names,
        );
    }
}

// ── OpenAPI registry: 400/403/500/503 declared ───────────────────────────

#[tokio::test]
async fn openapi_registry_declares_400_403_500_503_for_query_routes() {
    // The runtime 503 emission from `inst-agg-8c` / `inst-raw-8b` is produced
    // by the canonical Problem mapper at the handler boundary and MUST appear
    // in the OpenAPI document so generated clients handle it.
    use modkit::api::OpenApiRegistryImpl;

    let registry = OpenApiRegistryImpl::new();
    let svc = service_with_authz(Arc::new(OkPlugin), allow_authz());
    // Reuse the ingest runtime helper purely to satisfy `register_routes`'s
    // `runtime: Arc<dyn UsageEmitterRuntimeV1>` parameter; the routes-wiring
    // path under test never invokes it.
    let runtime = build_handler_runtime(Arc::new(StubAlwaysAllowAuthZ)).await;
    let _router =
        crate::api::rest::routes::register_routes(axum::Router::new(), &registry, runtime, svc);

    for path in [
        "GET:/usage-collector/v1/aggregated",
        "GET:/usage-collector/v1/raw",
    ] {
        let spec = registry
            .operation_specs
            .get(path)
            .unwrap_or_else(|| panic!("operation spec missing for {path}"));
        let declared: std::collections::HashSet<u16> =
            spec.responses.iter().map(|r| r.status).collect();
        for status in [400_u16, 403, 500, 503] {
            assert!(
                declared.contains(&status),
                "{path} must declare {status} in OpenAPI; got {declared:?}"
            );
        }
    }
}
