#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, DenyReason};
use axum::routing::{get, post};
use axum::{Extension, Router};
use chrono::{DateTime, Utc};
use modkit::client_hub::{ClientHub, ClientScope};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::outbox_migrations;
use modkit_db::{ConnectOpts, connect_db};
use modkit_odata::SortDir;
use modkit_security::SecurityContext;
use modkit_security::access_scope::pep_properties;
use types_registry_sdk::testing::make_test_instance;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, TypesRegistryError,
};
use usage_collector::api::rest::handlers::{
    handle_create_usage_record, handle_get_module_config, handle_query_aggregated, handle_query_raw,
};
use usage_collector::config::{MetricConfig, UsageCollectorConfig};
use usage_collector::domain::Service;
use usage_collector_sdk::{
    AggregationQuery, AggregationResult, AllowedMetric, CursorV1, ModuleConfig, Page, PageInfo,
    RawQuery, UsageCollectorClientV1, UsageCollectorError, UsageCollectorPluginClientV1,
    UsageCollectorPluginSpecV1, UsageKind, UsageRecord,
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

/// Configurable storage-plugin mock used by the integration test harness.
///
/// The default constructor mirrors the noop plugin: empty aggregated results,
/// empty raw `Page`. Builder-style helpers add additional behaviours used by
/// the Phase 6 integration tests: a paginated raw cursor walk, plugin failure
/// (canonical `ServiceUnavailable`), and `ResourceExhausted` propagation
/// through the canonical 503 path.
type RawPagesQueue = Arc<Mutex<Vec<Page<UsageRecord>>>>;

pub struct MockUsageCollectorPluginClientV1 {
    raw_pages: Option<RawPagesQueue>,
    raw_observed_page_size: Arc<Mutex<Option<u32>>>,
    fail_with_service_unavailable: bool,
    fail_with_resource_exhausted: bool,
}

impl MockUsageCollectorPluginClientV1 {
    pub fn new() -> Self {
        Self {
            raw_pages: None,
            raw_observed_page_size: Arc::new(Mutex::new(None)),
            fail_with_service_unavailable: false,
            fail_with_resource_exhausted: false,
        }
    }

    /// Configure the plugin to fail every `query_aggregated` / `query_raw` call
    /// with a canonical `ServiceUnavailable` error. The gateway must surface
    /// this as `503` carrying the request's `correlation_id` via the canonical
    /// Problem mapper (`inst-agg-8c` / `inst-raw-8b`).
    #[allow(dead_code)]
    pub fn service_unavailable() -> Self {
        Self {
            raw_pages: None,
            raw_observed_page_size: Arc::new(Mutex::new(None)),
            fail_with_service_unavailable: true,
            fail_with_resource_exhausted: false,
        }
    }

    /// Configure the plugin to fail every query with a canonical
    /// `ResourceExhausted` error so we can pin the planning decision
    /// "no 4xx shortcut for `ResourceExhausted`; route through the same
    /// canonical 503 path as every other non-Denied plugin error".
    #[allow(dead_code)]
    pub fn resource_exhausted() -> Self {
        Self {
            raw_pages: None,
            raw_observed_page_size: Arc::new(Mutex::new(None)),
            fail_with_service_unavailable: false,
            fail_with_resource_exhausted: true,
        }
    }

    /// Pre-seed a queue of `Page<UsageRecord>` to return from successive
    /// `query_raw` invocations. The first call returns `pages[0]`, the second
    /// returns `pages[1]`, etc. The final page MUST set `next_cursor=None`.
    ///
    /// Used to drive the multi-page cursor traversal test: the test calls the
    /// endpoint twice (round-trip the `CursorV1` from page 1 into the page 2
    /// request) and asserts both pages contribute records without duplicates
    /// or gaps.
    #[allow(dead_code)]
    pub fn with_raw_pages(pages: Vec<Page<UsageRecord>>) -> Self {
        Self {
            raw_pages: Some(Arc::new(Mutex::new(pages))),
            raw_observed_page_size: Arc::new(Mutex::new(None)),
            fail_with_service_unavailable: false,
            fail_with_resource_exhausted: false,
        }
    }

    /// Returns a clone of the latest `page_size` observed on a `query_raw`
    /// invocation. Used by the `DEFAULT_PAGE_SIZE` assertion to confirm the
    /// gateway resolved the absent caller-supplied `page_size` against the
    /// default.
    #[allow(dead_code)]
    pub fn observed_page_size(&self) -> Arc<Mutex<Option<u32>>> {
        Arc::clone(&self.raw_observed_page_size)
    }
}

#[async_trait]
impl UsageCollectorPluginClientV1 for MockUsageCollectorPluginClientV1 {
    async fn create_usage_record(&self, _: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn query_aggregated(
        &self,
        _query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        if self.fail_with_service_unavailable {
            return Err(UsageCollectorError::service_unavailable()
                .with_detail("plugin offline")
                .create());
        }
        if self.fail_with_resource_exhausted {
            return Err(usage_collector_sdk::UsageRecordError::resource_exhausted(
                "query result too large",
            )
            .with_quota_violation("rows", "row count exceeds limit")
            .create());
        }
        Ok(vec![])
    }

    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        *self.raw_observed_page_size.lock().unwrap() = Some(query.page_size);

        if self.fail_with_service_unavailable {
            return Err(UsageCollectorError::service_unavailable()
                .with_detail("plugin offline")
                .create());
        }
        if self.fail_with_resource_exhausted {
            return Err(usage_collector_sdk::UsageRecordError::resource_exhausted(
                "query result too large",
            )
            .with_quota_violation("rows", "row count exceeds limit")
            .create());
        }

        if let Some(pages) = &self.raw_pages {
            let mut guard = pages.lock().unwrap();
            if guard.is_empty() {
                return Ok(Page::empty(u64::from(query.page_size)));
            }
            return Ok(guard.remove(0));
        }
        Ok(Page::empty(u64::from(query.page_size)))
    }
}

// ── Cursor / page helpers ─────────────────────────────────────────────────────

/// Build a single-record `UsageRecord` whose `timestamp` is the supplied value.
#[allow(dead_code)]
pub fn make_usage_record(timestamp: DateTime<Utc>, metric: &str) -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id: Uuid::new_v4(),
        metric: metric.to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        subject: None,
        idempotency_key: Uuid::new_v4().to_string(),
        timestamp,
        metadata: None,
    }
}

/// Build a `CursorV1` whose first key element is the RFC 3339 timestamp the
/// gateway uses to enforce `cursor ∈ [from, to]`.
#[allow(dead_code)]
pub fn make_cursor_at(ts: DateTime<Utc>) -> CursorV1 {
    CursorV1 {
        k: vec![ts.to_rfc3339(), Uuid::new_v4().to_string()],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    }
}

/// Build a `Page<UsageRecord>` containing a single record at `ts`, carrying a
/// `next_cursor` whose timestamp equals `next_ts` (used to drive the multi-
/// page traversal test).
#[allow(dead_code)]
pub fn page_with_next(ts: DateTime<Utc>, next_ts: DateTime<Utc>, limit: u64) -> Page<UsageRecord> {
    let record = make_usage_record(ts, "test.gauge");
    let cursor = make_cursor_at(next_ts);
    Page::new(
        vec![record],
        PageInfo {
            next_cursor: Some(cursor.encode().expect("CursorV1 encode infallible")),
            prev_cursor: None,
            limit,
        },
    )
}

/// Build a terminal `Page<UsageRecord>` containing one record and `next_cursor=None`.
#[allow(dead_code)]
pub fn final_page(ts: DateTime<Utc>, limit: u64) -> Page<UsageRecord> {
    let record = make_usage_record(ts, "test.gauge");
    Page::new(
        vec![record],
        PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit,
        },
    )
}

/// Percent-encode a `DateTime<Utc>` for use in URL query strings. Replaces the
/// timezone `+` and the `:` characters so the gateway sees the exact RFC 3339
/// payload it would receive from a real HTTP client.
#[allow(dead_code)]
pub fn encode_dt(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339().replace('+', "%2B").replace(':', "%3A")
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
    build_service_with_authz(
        plugin,
        metrics,
        Arc::new(MockAuthZResolverClient::allow(Uuid::nil())) as Arc<dyn AuthZResolverClient>,
    )
}

/// Build a `Service` with an explicit PDP authz client. The query endpoints'
/// authorize-and-compile-scope algorithm threads this client into
/// `Service::query_aggregated` / `query_raw`, so tests can drive the 403
/// (`inst-agg-6a` / `inst-raw-6a`) and 503 (`inst-agg-8c` / `inst-raw-8b`)
/// arms by varying the PDP behaviour while keeping the plugin constant.
#[allow(dead_code)]
pub fn build_service_with_authz(
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
    metrics: HashMap<String, MetricConfig>,
    authz: Arc<dyn AuthZResolverClient>,
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
        authz,
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

    /// Integration-test harness for the query endpoints: arbitrary storage
    /// plugin behind a `Service` that delegates to the supplied PDP authz
    /// client. Both query routes are mounted so a single harness can drive
    /// `GET /aggregated` and `GET /raw`.
    #[allow(dead_code)]
    pub async fn with_query_plugin_and_authz(
        plugin: Arc<dyn UsageCollectorPluginClientV1>,
        authz: Arc<dyn AuthZResolverClient>,
    ) -> Self {
        let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz().await)
            as Arc<dyn UsageEmitterRuntimeV1>;
        let service = build_service_with_authz(plugin, HashMap::new(), Arc::clone(&authz));
        Self::build(runtime, service, authz)
    }

    /// Convenience constructor for query-endpoint happy-path tests: drives the
    /// supplied plugin behind a permissive PDP that returns a single tenant
    /// constraint, satisfying `require_constraints(true)`.
    #[allow(dead_code)]
    pub async fn with_query_plugin(plugin: Arc<dyn UsageCollectorPluginClientV1>) -> Self {
        let authz: Arc<dyn AuthZResolverClient> =
            Arc::new(MockAuthZResolverClient::allow(Uuid::new_v4()));
        Self::with_query_plugin_and_authz(plugin, authz).await
    }

    /// Convenience constructor for the `403 Forbidden` PDP-denied path. Uses a
    /// permissive emitter runtime so the ingest endpoints still work but the
    /// query endpoints fail closed on PDP denial.
    #[allow(dead_code)]
    pub async fn with_deny_authz() -> Self {
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolverClient::deny());
        Self::with_query_plugin_and_authz(plugin, authz).await
    }

    /// Convenience constructor for the `503 Service Unavailable` plugin-error
    /// path. The PDP allows the call so the handler reaches the plugin, which
    /// returns a canonical `ServiceUnavailable` mapped to 503 via the canonical
    /// Problem mapper (`inst-agg-8c` / `inst-raw-8b`).
    #[allow(dead_code)]
    pub async fn with_unavailable_plugin() -> Self {
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::service_unavailable());
        Self::with_query_plugin(plugin).await
    }

    /// Convenience constructor: `ResourceExhausted` plugin error. Used to pin
    /// the planning decision "no 400 shortcut; route through the canonical
    /// 503 path".
    #[allow(dead_code)]
    pub async fn with_resource_exhausted_plugin() -> Self {
        let plugin: Arc<dyn UsageCollectorPluginClientV1> =
            Arc::new(MockUsageCollectorPluginClientV1::resource_exhausted());
        Self::with_query_plugin(plugin).await
    }

    /// Build a harness around a plugin and PDP authz client with explicit
    /// `SecurityContext` (so the test can pin the `correlation_id` in the 503
    /// body / ERROR log assertions).
    #[allow(dead_code)]
    pub async fn with_query_plugin_authz_and_ctx(
        plugin: Arc<dyn UsageCollectorPluginClientV1>,
        authz: Arc<dyn AuthZResolverClient>,
        ctx: SecurityContext,
    ) -> Self {
        let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz().await)
            as Arc<dyn UsageEmitterRuntimeV1>;
        let service = build_service_with_authz(plugin, HashMap::new(), Arc::clone(&authz));
        Self::build_with_ctx(runtime, service, authz, ctx)
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
        Self::build_with_ctx(runtime, service, authz, ctx)
    }

    fn build_with_ctx(
        runtime: Arc<dyn UsageEmitterRuntimeV1>,
        service: Arc<Service>,
        authz: Arc<dyn AuthZResolverClient>,
        ctx: SecurityContext,
    ) -> Self {
        let router = Router::new()
            .route(
                "/usage-collector/v1/records",
                post(handle_create_usage_record),
            )
            .route(
                "/usage-collector/v1/modules/{module_name}/config",
                get(handle_get_module_config),
            )
            .route(
                "/usage-collector/v1/aggregated",
                get(handle_query_aggregated),
            )
            .route("/usage-collector/v1/raw", get(handle_query_raw))
            .layer(Extension(runtime))
            .layer(Extension(service))
            .layer(Extension(authz))
            .layer(Extension(ctx));
        Self { router }
    }
}
