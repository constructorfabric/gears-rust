//! Shared fakes for the gear's unit tests: PDP doubles, a recording metrics
//! sink, plugin fixtures that stand in for registered plugin instances, a
//! cluster wired over the standalone backend, and the metric and subject type
//! fixtures served by a platform-plane registry double.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    dead_code,
    reason = "test support"
)]

use crate::domain::ports::metrics::{
    CatalogReadPhase, EngineLabel, OperationKind, PolicyTransition, RetentionTable,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{
    Constraint, InPredicate, InTenantSubtreePredicate, Predicate,
};
use authz_resolver_sdk::models::{
    DenyReason, EvaluationRequest, EvaluationResponse, EvaluationResponseContext, Resource,
};
use cluster::{ClusterHandle, ClusterWiring, ProfileBackends};
use cluster_sdk::{
    ClusterError, ClusterProfile, ElectionConfig, LeaderElectionBackend, LeaderElectionFeatures,
    LeaderStatus, LeaderWatch,
};
use gts::GTS_ID_URI_PREFIX;
use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    QuotaEnforcementStoragePluginSpecV1, QuotaEnforcementStoragePluginV1, TenantId,
};
use quota_resolver_sdk::{QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID};
use serde_json::{Value, json};
use standalone_cluster_plugin::{StandaloneClusterHandle, StandaloneClusterPlugin};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::domain::legacy_local_client::TypesRegistryLocalClient;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};
use types_registry_sdk::{
    GtsInstance, PlatformTypesRegistryApi, RegisterResult, TypesRegistryClient,
};
use uuid::Uuid;

use crate::domain::catalog::MetricCatalog;
use crate::domain::error::DomainError;
use crate::domain::ports::coordination::{
    CoordinatorBinding, LeaderWork, SingletonCoordinator, SingletonScope,
};
use crate::domain::ports::lifecycle_gauges::{LifecycleCounts, LifecycleGaugeSink};
use crate::domain::ports::metrics::{
    DenialReason, MetricLabel, QeMetrics, SinkLabel, ValidationReason, ValidationSurface,
};
use crate::infra::cluster_coordination::QuotaEnforcementProfile;
use crate::infra::types_registry::PlatformTypeRegistry;

/// The tenant every fixture belongs to.
pub fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x7e57_0000_0000_0000_0000_0000_0000_0001))
}

/// An authenticated service principal in the fixture tenant.
pub fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("test security context")
}

static CALLER: std::sync::LazyLock<SecurityContext> = std::sync::LazyLock::new(ctx);

/// [`ctx`], borrowed for as long as a component built over it lives.
pub fn caller() -> &'static SecurityContext {
    &CALLER
}

/// The actor [`ctx`] is recorded as.
pub fn actor() -> quota_enforcement_sdk::Actor {
    quota_enforcement_sdk::Actor::Caller {
        subject_id: ctx().subject_id(),
    }
}

// ---------------------------------------------------------------------------
// PDP doubles
// ---------------------------------------------------------------------------

/// Permits every request with an `owner_tenant_id IN (tenants)` constraint.
pub struct PermitTenantsPdp {
    tenants: Vec<Uuid>,
    calls: AtomicUsize,
    last_resource_id: Mutex<Option<String>>,
    last_resource: Mutex<Option<Resource>>,
}

impl PermitTenantsPdp {
    pub fn new(tenants: Vec<Uuid>) -> Self {
        Self {
            tenants,
            calls: AtomicUsize::new(0),
            last_resource_id: Mutex::new(None),
            last_resource: Mutex::new(None),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn last_resource_id(&self) -> Option<String> {
        self.last_resource_id.lock().expect("lock").clone()
    }

    /// The `resource` of the last evaluation request, properties included.
    pub fn last_resource(&self) -> Option<Resource> {
        self.last_resource.lock().expect("lock").clone()
    }
}

#[async_trait]
impl AuthZResolverApi for PermitTenantsPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_resource_id.lock().expect("lock") = request.resource.id.map(|id| id.to_string());
        *self.last_resource.lock().expect("lock") = Some(request.resource);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.tenants.clone(),
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// Permits exactly one attribution tuple: every expected resource property must
/// be present with the expected value. Anything else is denied, so a changed
/// tenant, subject, metric, or resource is `PermissionDenied`.
pub struct TupleMatchingPdp {
    expected: serde_json::Map<String, Value>,
    tenants: Vec<Uuid>,
    calls: AtomicUsize,
}

impl TupleMatchingPdp {
    pub fn new(expected: serde_json::Map<String, Value>, tenants: Vec<Uuid>) -> Self {
        Self {
            expected,
            tenants,
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthZResolverApi for TupleMatchingPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let matches = self
            .expected
            .iter()
            .all(|(key, value)| request.resource.properties.get(key) == Some(value));
        if !matches {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(DenyReason {
                        error_code: "TUPLE_NOT_AUTHORIZED".to_owned(),
                        details: None,
                    }),
                },
            });
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.tenants.clone(),
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// A snapshot policy that inspects every target: it permits only when the
/// target tenant is one of `tenants` and each `{kind, id, metric}` of the
/// `filters` property is on its allow list, so one target outside the grant
/// denies the whole request.
pub struct PermitFiltersPdp {
    allowed: Vec<Value>,
    tenants: Vec<Uuid>,
    calls: AtomicUsize,
}

impl PermitFiltersPdp {
    /// Permit exactly the `(type, id, metric)` targets in `allowed`.
    pub fn new(allowed: &[(&str, &str, &str)], tenants: Vec<Uuid>) -> Self {
        Self {
            allowed: allowed
                .iter()
                .map(|(subject_type, id, metric)| serde_json::json!({ "type": subject_type, "id": id, "metric": metric }))
                .collect(),
            tenants,
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthZResolverApi for PermitFiltersPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let tenant_allowed = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .is_some_and(|owner| {
                self.tenants
                    .iter()
                    .any(|tenant| serde_json::to_value(tenant).ok().as_ref() == Some(owner))
            });
        let every_target_allowed = tenant_allowed
            && request
                .resource
                .properties
                .get(crate::domain::pep::properties::FILTERS)
                .and_then(Value::as_array)
                .is_some_and(|filters| {
                    !filters.is_empty()
                        && filters.iter().all(|filter| self.allowed.contains(filter))
                });
        if !every_target_allowed {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(DenyReason {
                        error_code: "TARGET_NOT_AUTHORIZED".to_owned(),
                        details: None,
                    }),
                },
            });
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.tenants.clone(),
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// Permits every request with an `owner_tenant_id IN SUBTREE(root)` constraint,
/// the shape a hierarchy-aware PDP returns. The filter has no literal values in
/// memory; only `SecureConn` can evaluate it.
pub struct PermitSubtreePdp {
    root: Uuid,
}

impl PermitSubtreePdp {
    pub fn new(root: Uuid) -> Self {
        Self { root }
    }
}

#[async_trait]
impl AuthZResolverApi for PermitSubtreePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.root,
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// Permits without any constraint. Under `require_constraints` the PEP must
/// fail closed on it.
pub struct PermitUnconstrainedPdp;

#[async_trait]
impl AuthZResolverApi for PermitUnconstrainedPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Denies every request.
pub struct DenyAllPdp;

#[async_trait]
impl AuthZResolverApi for DenyAllPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: Some(DenyReason {
                    error_code: "NO_GRANT".to_owned(),
                    details: None,
                }),
            },
        })
    }
}

/// A PDP that fails the test when anything calls it.
pub struct UncallablePdp;

#[async_trait]
impl AuthZResolverApi for UncallablePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        panic!("the PDP was called for {}", request.action.name);
    }
}

/// Unreachable PDP.
pub struct FailingPdp;

#[async_trait]
impl AuthZResolverApi for FailingPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("PDP unavailable").create())
    }
}

/// A PDP that never answers.
pub struct HangingPdp;

#[async_trait]
impl AuthZResolverApi for HangingPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        std::future::pending().await
    }
}

/// Registers a PDP double as the `authz-resolver` client.
pub fn register_pdp(hub: &Arc<ClientHub>, pdp: Arc<dyn AuthZResolverApi>) {
    hub.register::<dyn AuthZResolverApi>(pdp);
}

// ---------------------------------------------------------------------------
// Metrics double
// ---------------------------------------------------------------------------

/// Records every emission in order.
#[derive(Default)]
pub struct RecordingMetrics {
    pub engine_bootstrap_failures: parking_lot::Mutex<Vec<EngineLabel>>,
    pub engine_evaluations: parking_lot::Mutex<Vec<(EngineLabel, std::time::Duration)>>,
    pub plan_violations: parking_lot::Mutex<
        Vec<(
            EngineLabel,
            quota_enforcement_sdk::engine::DebitPlanInvariant,
        )>,
    >,
    pub policy_transitions: parking_lot::Mutex<Vec<PolicyTransition>>,
    pub policy_conflicts: parking_lot::Mutex<u64>,
    denials: Mutex<Vec<DenialReason>>,
    contract_failures: Mutex<Vec<(ValidationSurface, ValidationReason)>>,
    admitted_violations: Mutex<Vec<ValidationSurface>>,
    evaluations: Mutex<Vec<(OperationKind, std::time::Duration)>>,
    replays: Mutex<Vec<OperationKind>>,
    contention: Mutex<Vec<OperationKind>>,
    reclaimed: Mutex<Vec<(RetentionTable, u64)>>,
    retention_failures: Mutex<Vec<RetentionTable>>,
    lease_waits: Mutex<Vec<String>>,
    lease_contention: Mutex<Vec<String>>,
    lease_cap: Mutex<Vec<String>>,
    dispatch_failures: Mutex<Vec<(String, quota_enforcement_sdk::NotificationEventKind)>>,
    outbox_rejections: Mutex<u64>,
    policy_evaluation_failures: Mutex<Vec<String>>,
    catalog_read_failures: Mutex<Vec<CatalogReadPhase>>,
    catalog_scheduled_reads: Mutex<u64>,
    catalog_unavailable: Mutex<Option<u64>>,
}

impl RecordingMetrics {
    /// `notification_dispatch_failures_total` emissions as `(sink_id, kind)`.
    pub fn dispatch_failures(&self) -> Vec<(String, quota_enforcement_sdk::NotificationEventKind)> {
        self.dispatch_failures.lock().expect("lock").clone()
    }

    /// `outbox_rejections_total` so far.
    pub fn outbox_rejections(&self) -> u64 {
        *self.outbox_rejections.lock().expect("lock")
    }

    /// Metrics whose lease acquisition latency was observed, in order.
    pub fn lease_waits(&self) -> Vec<String> {
        self.lease_waits.lock().expect("lock").clone()
    }

    /// Operations refused on contention, in order.
    pub fn contention(&self) -> Vec<OperationKind> {
        self.contention.lock().expect("lock").clone()
    }

    /// Metrics of debits and lease acquisitions refused on contention, in order.
    pub fn lease_contention(&self) -> Vec<String> {
        self.lease_contention.lock().expect("lock").clone()
    }

    /// Metrics of acquisitions refused on the active-lease cap, in order.
    pub fn lease_cap(&self) -> Vec<String> {
        self.lease_cap.lock().expect("lock").clone()
    }

    pub fn denials(&self) -> Vec<DenialReason> {
        self.denials.lock().expect("lock").clone()
    }

    /// Operations whose latency was observed, in order.
    pub fn evaluations(&self) -> Vec<OperationKind> {
        self.evaluations
            .lock()
            .expect("lock")
            .iter()
            .map(|(operation, _)| *operation)
            .collect()
    }

    /// Operations that answered from a stored record.
    pub fn replays(&self) -> Vec<OperationKind> {
        self.replays.lock().expect("lock").clone()
    }

    /// Rows reclaimed per table, in order.
    pub fn reclaimed(&self) -> Vec<(RetentionTable, u64)> {
        self.reclaimed.lock().expect("lock").clone()
    }

    /// Tables whose sweep failed.
    pub fn retention_failures(&self) -> Vec<RetentionTable> {
        self.retention_failures.lock().expect("lock").clone()
    }

    /// `contract_validation_failures_total` emissions as `(surface, reason)`.
    pub fn contract_failures(&self) -> Vec<(ValidationSurface, ValidationReason)> {
        self.contract_failures.lock().expect("lock").clone()
    }

    /// `admitted_metric_violations_total` emissions by surface.
    pub fn admitted_violations(&self) -> Vec<ValidationSurface> {
        self.admitted_violations.lock().expect("lock").clone()
    }

    /// Metrics of `policy_evaluation_failures_total` emissions, in order.
    pub fn policy_evaluation_failures(&self) -> Vec<String> {
        self.policy_evaluation_failures
            .lock()
            .expect("lock")
            .clone()
    }

    /// `catalog_read_failures_total` emissions by phase.
    pub fn catalog_read_failures(&self) -> Vec<CatalogReadPhase> {
        self.catalog_read_failures.lock().expect("lock").clone()
    }

    /// Scheduled catalogue reads that succeeded.
    pub fn catalog_scheduled_reads(&self) -> u64 {
        *self.catalog_scheduled_reads.lock().expect("lock")
    }

    /// The last `catalog_unavailable_metrics` value.
    pub fn catalog_unavailable(&self) -> Option<u64> {
        *self.catalog_unavailable.lock().expect("lock")
    }
}

impl QeMetrics for RecordingMetrics {
    fn record_engine_bootstrap_failure(&self, engine: EngineLabel) {
        self.engine_bootstrap_failures.lock().push(engine);
    }
    fn record_engine_evaluation(&self, engine: EngineLabel, elapsed: std::time::Duration) {
        self.engine_evaluations.lock().push((engine, elapsed));
    }
    fn record_plan_violation(
        &self,
        engine: EngineLabel,
        invariant: quota_enforcement_sdk::engine::DebitPlanInvariant,
    ) {
        self.plan_violations.lock().push((engine, invariant));
    }
    fn record_policy_transition(&self, transition: PolicyTransition) {
        self.policy_transitions.lock().push(transition);
    }
    fn record_policy_conflict(&self) {
        *self.policy_conflicts.lock() += 1;
    }

    fn record_denial(&self, reason: DenialReason) {
        self.denials.lock().expect("lock").push(reason);
    }

    fn record_contract_validation_failure(
        &self,
        surface: ValidationSurface,
        reason: ValidationReason,
    ) {
        self.contract_failures
            .lock()
            .expect("lock")
            .push((surface, reason));
    }

    fn record_admitted_metric_violation(&self, surface: ValidationSurface) {
        self.admitted_violations.lock().expect("lock").push(surface);
    }

    fn record_evaluation(&self, operation: OperationKind, elapsed: std::time::Duration) {
        self.evaluations
            .lock()
            .expect("lock")
            .push((operation, elapsed));
    }

    fn record_idempotency_replay(&self, operation: OperationKind) {
        self.replays.lock().expect("lock").push(operation);
    }

    fn record_contention_rejected(&self, operation: OperationKind) {
        self.contention.lock().expect("lock").push(operation);
    }

    fn record_retention_reclaimed(&self, table: RetentionTable, rows: u64) {
        self.reclaimed.lock().expect("lock").push((table, rows));
    }

    fn record_retention_failure(&self, table: RetentionTable) {
        self.retention_failures.lock().expect("lock").push(table);
    }

    fn record_lease_acquisition_wait(&self, metric: &MetricLabel, _elapsed: std::time::Duration) {
        self.lease_waits
            .lock()
            .expect("lock")
            .push(metric.as_str().to_owned());
    }

    fn record_lease_contention_rejected(&self, metric: &MetricLabel) {
        self.lease_contention
            .lock()
            .expect("lock")
            .push(metric.as_str().to_owned());
    }

    fn record_lease_inflight_limit_exceeded(&self, metric: &MetricLabel) {
        self.lease_cap
            .lock()
            .expect("lock")
            .push(metric.as_str().to_owned());
    }

    fn record_notification_dispatch_failure(
        &self,
        sink: &SinkLabel,
        kind: quota_enforcement_sdk::NotificationEventKind,
    ) {
        self.dispatch_failures
            .lock()
            .expect("lock")
            .push((sink.as_str().to_owned(), kind));
    }

    fn record_outbox_rejection(&self) {
        *self.outbox_rejections.lock().expect("lock") += 1;
    }

    fn record_policy_evaluation_failure(&self, _engine: EngineLabel, metric: &MetricLabel) {
        self.policy_evaluation_failures
            .lock()
            .expect("lock")
            .push(metric.as_str().to_owned());
    }

    fn record_catalog_read_failure(&self, phase: CatalogReadPhase) {
        self.catalog_read_failures.lock().expect("lock").push(phase);
    }

    fn record_catalog_scheduled_read(&self) {
        *self.catalog_scheduled_reads.lock().expect("lock") += 1;
    }

    fn set_catalog_unavailable_metrics(&self, count: u64) {
        *self.catalog_unavailable.lock().expect("lock") = Some(count);
    }
}

// ---------------------------------------------------------------------------
// Plugin fixtures
// ---------------------------------------------------------------------------

/// A registered plugin instance as the types registry would list it.
pub struct PluginFixture {
    /// Full GTS instance id.
    pub instance_id: String,
    /// Registry entity.
    pub entity: GtsInstance,
}

/// A storage plugin instance.
pub fn storage_instance(segment: &str, vendor: &str, priority: i16) -> PluginFixture {
    let (id, payload) = PluginV1::<QuotaEnforcementStoragePluginSpecV1>::build_registration(
        segment, vendor, priority,
    )
    .expect("registration payload");
    PluginFixture {
        instance_id: id.to_string(),
        entity: make_test_instance(id.as_ref(), payload),
    }
}

/// A notification sink plugin instance.
pub fn sink_instance(segment: &str, vendor: &str) -> PluginFixture {
    let (id, payload) =
        PluginV1::<quota_enforcement_sdk::QuotaNotificationSinkSpecV1>::build_registration(
            segment, vendor, 0,
        )
        .expect("registration payload");
    PluginFixture {
        instance_id: id.to_string(),
        entity: make_test_instance(id.as_ref(), payload),
    }
}

impl PluginFixture {
    /// A sink instance whose content is not a plugin spec.
    pub fn malformed_sink(segment: &str) -> Self {
        let (id, _) =
            PluginV1::<quota_enforcement_sdk::QuotaNotificationSinkSpecV1>::build_registration(
                segment, "acme", 0,
            )
            .expect("registration payload");
        let broken = json!({ "id": id.to_string(), "vendor": 7 });
        Self {
            instance_id: id.to_string(),
            entity: make_test_instance(id.as_ref(), broken),
        }
    }

    /// A storage instance whose content is not a plugin spec.
    pub fn malformed_storage(segment: &str) -> Self {
        let (id, _) =
            PluginV1::<QuotaEnforcementStoragePluginSpecV1>::build_registration(segment, "acme", 1)
                .expect("registration payload");
        let broken = json!({ "id": id.to_string(), "priority": "not-a-number" });
        Self {
            instance_id: id.to_string(),
            entity: make_test_instance(id.as_ref(), broken),
        }
    }
}

/// A hub whose legacy types registry lists `fixtures` and whose platform
/// registry is [`platform_registry`].
pub fn hub_with(fixtures: &[&PluginFixture]) -> Arc<ClientHub> {
    let registry =
        MockTypesRegistryClient::new().with_instances(fixtures.iter().map(|f| f.entity.clone()));
    let hub = Arc::new(ClientHub::new());
    let client: Arc<dyn TypesRegistryClient> = Arc::new(registry);
    hub.register::<dyn TypesRegistryClient>(client);
    platform_registry().install(&hub);
    hub
}

/// A hub whose types registry fails every listing with `err`.
pub fn hub_with_failing_registry(err: CanonicalError) -> Arc<ClientHub> {
    let registry = MockTypesRegistryClient::new().with_list_error(err);
    let hub = Arc::new(ClientHub::new());
    let client: Arc<dyn TypesRegistryClient> = Arc::new(registry);
    hub.register::<dyn TypesRegistryClient>(client);
    hub
}

/// Registers a storage double as the scoped client of `fixture`.
pub fn register_storage(
    hub: &Arc<ClientHub>,
    fixture: &PluginFixture,
    storage: Arc<InMemoryStorage>,
) {
    let api: Arc<dyn QuotaEnforcementStoragePluginV1> = storage;
    hub.register_scoped::<dyn QuotaEnforcementStoragePluginV1>(
        ClientScope::gts_id(&fixture.instance_id),
        api,
    );
}

/// Registers `sink` as the scoped client of `fixture`.
pub fn register_sink(
    hub: &Arc<ClientHub>,
    fixture: &PluginFixture,
    sink: Arc<dyn quota_enforcement_sdk::QuotaNotificationSinkV1>,
) {
    hub.register_scoped::<dyn quota_enforcement_sdk::QuotaNotificationSinkV1>(
        ClientScope::gts_id(&fixture.instance_id),
        sink,
    );
}

// ---------------------------------------------------------------------------
// Coordination doubles (domain tests)
// ---------------------------------------------------------------------------

/// A coordinator that never leads: it returns once `shutdown` fires.
pub struct NoopCoordinator;

#[async_trait]
impl SingletonCoordinator for NoopCoordinator {
    async fn run_while_leader(
        &self,
        _scope: SingletonScope,
        shutdown: CancellationToken,
        _work: LeaderWork,
    ) -> Result<(), DomainError> {
        shutdown.cancelled().await;
        Ok(())
    }
}

/// A binding that resolves to [`NoopCoordinator`], or fails with the injected
/// error. Counts the resolve calls.
pub struct StaticCoordinatorBinding {
    failure: Option<DomainError>,
    calls: AtomicUsize,
}

impl StaticCoordinatorBinding {
    /// Resolves successfully.
    pub fn ok() -> Arc<Self> {
        Arc::new(Self {
            failure: None,
            calls: AtomicUsize::new(0),
        })
    }

    /// Fails every resolve with `err`.
    pub fn failing(err: DomainError) -> Arc<Self> {
        Arc::new(Self {
            failure: Some(err),
            calls: AtomicUsize::new(0),
        })
    }

    /// Number of resolve calls.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CoordinatorBinding for StaticCoordinatorBinding {
    async fn resolve(&self) -> Result<Arc<dyn SingletonCoordinator>, DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.failure {
            Some(err) => Err(err.clone()),
            None => Ok(Arc::new(NoopCoordinator)),
        }
    }
}

/// A sweep body that does nothing and returns at once.
pub fn idle_work() -> LeaderWork {
    Arc::new(|_token: CancellationToken| Box::pin(async {}))
}

// ---------------------------------------------------------------------------
// Cluster fixture (infra and gear tests)
// ---------------------------------------------------------------------------

/// A cluster wired into a hub over the standalone backend. Stop it at the end
/// of the test: the cluster handle panics in debug builds when dropped without
/// `stop()`.
pub struct ClusterFixture {
    cluster: ClusterHandle,
    standalone: StandaloneClusterHandle,
}

impl ClusterFixture {
    /// Deregisters the backends and stops the standalone sweeper.
    pub async fn stop(self) {
        self.cluster.stop().await;
        self.standalone.stop().await;
    }
}

/// A profile the gear never resolves.
#[derive(Debug, Clone, Copy)]
pub struct OtherProfile;

impl ClusterProfile for OtherProfile {
    const NAME: &'static str = "other";
}

/// Wires the standalone cache under the `quota-enforcement` profile. Leader
/// election is the SDK default over that cache, which is linearizable.
pub fn wire_cluster(hub: &Arc<ClientHub>) -> ClusterFixture {
    wire_cluster_with(hub, QuotaEnforcementProfile, None)
}

/// Wires the standalone cache under `profile`, with an explicit leader-election
/// backend when `leader` is given.
pub fn wire_cluster_with<P: ClusterProfile>(
    hub: &Arc<ClientHub>,
    profile: P,
    leader: Option<Arc<dyn LeaderElectionBackend>>,
) -> ClusterFixture {
    let standalone = StandaloneClusterPlugin::builder()
        .build_and_start()
        .expect("standalone cluster backend");
    let mut backends = ProfileBackends::new(standalone.cache());
    if let Some(leader) = leader {
        backends = backends.with_leader_election(leader);
    }
    let cluster = ClusterWiring::builder(hub.clone())
        .profile(profile, backends)
        .build_and_start()
        .expect("cluster wiring");
    ClusterFixture {
        cluster,
        standalone,
    }
}

/// A leader-election backend that declares no linearizable election. It never
/// elects anyone; it exists to fail the `Linearizable` requirement at resolve.
pub struct AdvisoryOnlyLeader;

#[async_trait]
impl LeaderElectionBackend for AdvisoryOnlyLeader {
    fn features(&self) -> LeaderElectionFeatures {
        LeaderElectionFeatures::new(false)
    }

    async fn elect(&self, _name: &str) -> Result<LeaderWatch, ClusterError> {
        let (_sender, _resigns, watch) = LeaderWatch::channel(1, LeaderStatus::Follower);
        Ok(watch)
    }

    async fn elect_with_config(
        &self,
        name: &str,
        _config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        self.elect(name).await
    }
}

// ---------------------------------------------------------------------------
// Metric and subject type fixtures (ADR-0008, ADR-0013)
// ---------------------------------------------------------------------------

/// The JSON Schema dialect every GTS type declares.
pub const DRAFT7: &str = "http://json-schema.org/draft-07/schema#";

/// An operator's user subject type, with any non-empty id.
pub const USER_SUBJECT_TYPE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~";
/// An operator's device subject type, with any non-empty id.
pub const DEVICE_SUBJECT_TYPE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.device.v1~";
/// The tenant subject type of the quota resolver SDK.
pub const TENANT_SUBJECT_TYPE: &str = quota_resolver_sdk::QUOTA_TENANT_SUBJECT_TYPE_ID;

/// A counter that admits the user type; its metadata is `{region?, model?}`.
pub const METRIC_TOKENS: &str =
    "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~";
/// A counter that admits every subject type; its metadata is empty.
pub const METRIC_REQUESTS: &str =
    "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~";
/// A metric type that is never registered.
pub const METRIC_OTHER: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.other.v1~";

/// A concrete quota subject type `id` whose subject id is any non-empty string.
pub fn subject_type_document(id: &str) -> Value {
    json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{id}"),
        "$schema": DRAFT7,
        "type": "object",
        "allOf": [
            { "$ref": format!("{GTS_ID_URI_PREFIX}{QUOTA_SUBJECT_TYPE_ID}") },
            { "type": "object", "properties": { "id": { "type": "string", "minLength": 1 } } }
        ]
    })
}

/// A metric type `id` with `traits` and the closed `metadata` properties
/// `properties`.
pub fn metric_type_document(id: &str, traits: Value, properties: Value) -> Value {
    json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{id}"),
        "$schema": DRAFT7,
        "x-gts-traits": traits,
        "type": "object",
        "allOf": [
            { "$ref": format!("{GTS_ID_URI_PREFIX}{QUOTA_METRIC_TYPE_ID}") },
            {
                "type": "object",
                "properties": {
                    "metadata": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": properties
                    }
                }
            }
        ]
    })
}

/// The fixture types: the user and device subject types and the two metrics.
pub fn fixture_documents() -> Vec<Value> {
    vec![
        subject_type_document(USER_SUBJECT_TYPE),
        subject_type_document(DEVICE_SUBJECT_TYPE),
        metric_type_document(
            METRIC_TOKENS,
            json!({ "kind": "counter", "admitted_subjects": [USER_SUBJECT_TYPE] }),
            json!({ "region": { "type": "string" }, "model": { "type": "string" } }),
        ),
        metric_type_document(
            METRIC_REQUESTS,
            json!({ "kind": "counter", "admitted_subjects": [QUOTA_SUBJECT_TYPE_ID] }),
            json!({}),
        ),
    ]
}

/// The GTS id of a type document.
pub fn document_id(document: &Value) -> String {
    let uri = document["$id"]
        .as_str()
        .expect("a type document carries $id");
    uri.strip_prefix(GTS_ID_URI_PREFIX)
        .unwrap_or(uri)
        .to_owned()
}

/// A platform-plane registry double holding this binary's inventory (the
/// quota resolver SDK's bases among it) and the fixture types.
pub fn platform_registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    for document in fixture_documents() {
        registry.seed(&document_id(&document), document);
    }
    registry
}

/// A catalogue over `registry`, through the production adapter.
pub fn catalog_over(
    registry: &Arc<MockTypesRegistry>,
    metrics: Arc<dyn QeMetrics>,
) -> Arc<MetricCatalog> {
    Arc::new(MetricCatalog::new(
        Arc::new(PlatformTypeRegistry::new(
            Arc::clone(registry) as Arc<dyn PlatformTypesRegistryApi>,
            std::time::Duration::from_secs(5),
        )),
        metrics,
        crate::config::CatalogSection::default().unknown_memory(),
    ))
}

/// A catalogue over [`platform_registry`], and that registry.
pub fn test_catalog() -> (Arc<MetricCatalog>, Arc<MockTypesRegistry>) {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    (catalog, registry)
}

/// Every later registry read fails as unavailable.
pub fn fail_registry_reads(registry: &MockTypesRegistry) {
    registry.inject(
        Fault::on(Call::BatchGet).fail(
            CanonicalError::service_unavailable()
                .with_detail("registry down")
                .create(),
        ),
    );
}

/// Registry reads made so far.
pub fn registry_reads(registry: &MockTypesRegistry) -> u32 {
    registry.platform_calls(Call::BatchGet)
}

/// A real in-process `types-registry` behind the legacy client the plugin
/// binding reads, seeded in configuration phase with the process inventory
/// plus `extra`, then switched to ready mode.
pub fn in_process_registry(extra: Vec<Value>) -> Arc<dyn TypesRegistryClient> {
    let config = TypesRegistryConfig::default();
    let repository = Arc::new(InMemoryGtsRepository::new(config.to_gts_config()));
    let service = Arc::new(TypesRegistryService::new(repository, config));
    let mut entries = toolkit::gts::all_inventory_type_schemas().expect("inventory type schemas");
    entries.extend(toolkit::gts::all_inventory_instances().expect("inventory instances"));
    entries.extend(extra);
    let results = service.register(entries);
    RegisterResult::ensure_all_ok(&results).expect("fixture entities register");
    service
        .switch_to_ready()
        .expect("fixture registry switches to ready");
    Arc::new(TypesRegistryLocalClient::new(service))
}

/// A hub whose legacy types registry is `registry` and whose platform
/// registry is [`platform_registry`].
pub fn hub_with_registry(registry: Arc<dyn TypesRegistryClient>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(registry);
    platform_registry().install(&hub);
    hub
}

/// The plugin instance document of `fixture` as the registry accepts it.
pub fn plugin_instance_document(fixture: &PluginFixture) -> Value {
    fixture.entity.object.clone()
}

// ---------------------------------------------------------------------------
// Gauge doubles (quota-lifecycle)
// ---------------------------------------------------------------------------

/// Records every publication in order, withdrawals included.
#[derive(Default)]
pub struct RecordingGaugeSink {
    publications: Mutex<Vec<Option<LifecycleCounts>>>,
}

impl RecordingGaugeSink {
    pub fn publications(&self) -> Vec<Option<LifecycleCounts>> {
        self.publications.lock().expect("lock").clone()
    }

    /// The most recent publication, if any happened: `Some(None)` is a
    /// withdrawal, `None` means nothing was published yet.
    #[allow(
        clippy::option_option,
        reason = "a withdrawal is a publication of nothing"
    )]
    pub fn last(&self) -> Option<Option<LifecycleCounts>> {
        self.publications.lock().expect("lock").last().copied()
    }
}

impl LifecycleGaugeSink for RecordingGaugeSink {
    fn publish(&self, counts: Option<LifecycleCounts>) {
        self.publications.lock().expect("lock").push(counts);
    }
}

// ---------------------------------------------------------------------------
// A bound service for the transport tests (REST, in-process)
// ---------------------------------------------------------------------------

/// The lifecycle bounds every transport test runs with.
pub fn test_limits() -> crate::domain::quotas::QuotaLimits {
    crate::domain::quotas::QuotaLimits {
        metadata_max_bytes: 4096,
        list_max_limit: 500,
        list_max_ids: 100,
        bulk_max_items: 50,
    }
}

/// The configured policy bounds every test runs with: the shipped defaults.
pub fn policy_limits() -> crate::domain::policies::PolicyRuntimeLimits {
    crate::config::PoliciesSection::default()
        .to_limits()
        .expect("the default policy section is valid")
}

/// A service whose dependencies are not bound: every lifecycle call is
/// `NotReady`.
pub fn unbound_service(pdp: Arc<dyn AuthZResolverApi>) -> Arc<crate::domain::Service> {
    let metrics = Arc::new(RecordingMetrics::default());
    Arc::new(crate::domain::Service::new(
        crate::domain::Admission::new(authz_resolver_sdk::PolicyEnforcer::new(pdp), metrics),
        Arc::new(crate::domain::Readiness::new()),
        test_limits(),
        policy_limits(),
        crate::domain::service::OperationsRuntime {
            cache_entries: 16,
            cache_ttl: std::time::Duration::from_secs(5),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
            snapshot: crate::domain::operations::SnapshotLimits::default(),
        },
    ))
}

/// A service bound to the in-memory storage, seeded with the
/// `most-restrictive-wins` global policy, and a catalogue over
/// [`platform_registry`].
pub async fn bound_service(pdp: Arc<dyn AuthZResolverApi>) -> Arc<crate::domain::Service> {
    bound_service_over(pdp, mrw_storage().await).await
}

/// The in-memory storage bootstrapped with the `most-restrictive-wins` global
/// policy, under a timeout no runner reaches.
pub async fn mrw_storage() -> Arc<InMemoryStorage> {
    let storage = Arc::new(InMemoryStorage::new());
    let mut bundle = quota_enforcement_sdk::testing::bundle_with_global_policy();
    if let Some(policy) = bundle.global_policy.as_mut() {
        "most-restrictive-wins".clone_into(&mut policy.engine_id);
        policy.timeout_ms = Some(60_000);
    }
    storage.bootstrap(&bundle).await.expect("bootstrap");
    storage
}

/// The same service over a caller-supplied storage double, so a test can seed
/// Quotas and read counters through it.
pub async fn bound_service_over(
    pdp: Arc<dyn AuthZResolverApi>,
    storage: Arc<InMemoryStorage>,
) -> Arc<crate::domain::Service> {
    bound_service_with_registry(pdp, storage, &platform_registry()).await
}

/// [`bound_service_over`] with its catalogue over `registry`, so a test can
/// seed or break the metric types it reads.
pub async fn bound_service_with_registry(
    pdp: Arc<dyn AuthZResolverApi>,
    storage: Arc<InMemoryStorage>,
    registry: &Arc<MockTypesRegistry>,
) -> Arc<crate::domain::Service> {
    let service = unbound_service(pdp);
    let catalog = catalog_over(registry, Arc::new(RecordingMetrics::default()));
    service
        .bind(crate::domain::Bound {
            engines: Arc::new(crate::domain::engines::linked_registry().expect("engines")),
            artifacts: Arc::new(crate::domain::engines::PolicyArtifactCache::new(
                std::num::NonZeroUsize::new(256).expect("capacity"),
                std::num::NonZeroUsize::new(2).expect("permits"),
            )),
            storage,
            coordinator: Arc::new(NoopCoordinator),
            catalog,
        })
        .expect("bind");
    service
}

/// One opened span: its name, its parent's name, and its `item_count` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSpan {
    pub name: String,
    pub parent: Option<String>,
    pub item_count: Option<u64>,
}

/// What one capture recorded.
type Records = Arc<parking_lot::Mutex<Vec<CapturedSpan>>>;

/// The root span a capture runs its calls under.
const CAPTURE_ROOT: &str = "quota_enforcement_test.capture";

/// The live captures, by the id their root spans carry.
type LiveCaptures =
    parking_lot::Mutex<HashMap<u64, std::sync::Weak<parking_lot::Mutex<Vec<CapturedSpan>>>>>;

static CAPTURES: std::sync::LazyLock<LiveCaptures> = std::sync::LazyLock::new(Default::default);

/// Records every `quota_enforcement.*` span opened under its root, through one global
/// subscriber: a thread-local one let parallel `cargo test` cache spans as disabled.
#[derive(Clone)]
pub struct SpanCapture {
    id: u64,
    records: Records,
}

impl Default for SpanCapture {
    fn default() -> Self {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        INSTALL.call_once(|| {
            use tracing_subscriber::layer::SubscriberExt as _;
            tracing::subscriber::set_global_default(
                tracing_subscriber::registry().with(CaptureRouter),
            )
            .expect("the span capture is the test binary's global subscriber");
        });
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let records = Records::default();
        CAPTURES.lock().insert(id, Arc::downgrade(&records));
        Self { id, records }
    }
}

impl SpanCapture {
    fn root(&self) -> tracing::Span {
        tracing::info_span!(CAPTURE_ROOT, capture = self.id)
    }

    /// Run `call` under this capture's root. A task it spawns is captured
    /// only if it carries the span along, as production tracing needs too.
    pub async fn within<F: std::future::Future>(&self, call: F) -> F::Output {
        use tracing::Instrument as _;
        call.instrument(self.root()).await
    }

    /// Run `call` under this capture's root, on this thread.
    pub fn in_scope<T>(&self, call: impl FnOnce() -> T) -> T {
        self.root().in_scope(call)
    }

    pub fn spans(&self) -> Vec<CapturedSpan> {
        self.records.lock().clone()
    }

    pub fn names(&self) -> Vec<String> {
        self.spans().into_iter().map(|s| s.name).collect()
    }

    pub fn parent_of(&self, name: &str) -> Option<String> {
        self.spans()
            .into_iter()
            .find(|s| s.name == name)
            .and_then(|s| s.parent)
    }

    pub fn clear(&self) {
        self.records.lock().clear();
    }
}

/// One `u64` field of a span, by name.
struct U64Field {
    name: &'static str,
    value: Option<u64>,
}

impl tracing::field::Visit for U64Field {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == self.name {
            self.value = Some(value);
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

fn u64_field(attrs: &tracing::span::Attributes<'_>, name: &'static str) -> Option<u64> {
    let mut field = U64Field { name, value: None };
    attrs.record(&mut field);
    field.value
}

/// Hands each `quota_enforcement.*` span to the capture whose root it opens under.
struct CaptureRouter;

impl<S> tracing_subscriber::Layer<S> for CaptureRouter
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let name = attrs.metadata().name();
        if name == CAPTURE_ROOT {
            let records = u64_field(attrs, "capture").and_then(|capture| {
                CAPTURES
                    .lock()
                    .get(&capture)
                    .and_then(std::sync::Weak::upgrade)
            });
            if let Some(records) = records {
                span.extensions_mut().insert(records);
            }
            return;
        }
        if !name.starts_with("quota_enforcement.") {
            return;
        }
        let Some(records) = span
            .scope()
            .skip(1)
            .find_map(|ancestor| ancestor.extensions().get::<Records>().cloned())
        else {
            return;
        };
        let parent = span
            .parent()
            .filter(|parent| parent.name() != CAPTURE_ROOT)
            .map(|parent| parent.name().to_owned());
        records.lock().push(CapturedSpan {
            name: name.to_owned(),
            parent,
            item_count: u64_field(attrs, "item_count"),
        });
    }
}

/// One callsite for both threads: interest is cached per callsite.
fn capture_probe() -> tracing::Span {
    tracing::info_span!("quota_enforcement.capture_probe")
}

#[test]
fn a_span_first_opened_on_another_thread_is_still_captured() {
    let capture = SpanCapture::default();
    std::thread::spawn(|| drop(capture_probe()))
        .join()
        .expect("probe thread");
    capture.in_scope(|| drop(capture_probe()));
    assert_eq!(
        capture.names(),
        vec!["quota_enforcement.capture_probe".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_opened_in_a_worker_task_under_the_capture_is_captured() {
    use tracing::Instrument as _;
    let capture = SpanCapture::default();
    capture
        .within(async {
            tokio::spawn(
                async {
                    drop(tracing::info_span!(
                        "quota_enforcement.capture_worker_probe"
                    ));
                }
                .in_current_span(),
            )
            .await
            .expect("worker task");
        })
        .await;
    assert_eq!(
        capture.names(),
        vec!["quota_enforcement.capture_worker_probe".to_owned()]
    );
}

#[test]
fn each_capture_records_only_the_spans_under_its_own_root() {
    let (mine, other) = (SpanCapture::default(), SpanCapture::default());
    mine.in_scope(|| drop(capture_probe()));
    assert_eq!(
        mine.names(),
        vec!["quota_enforcement.capture_probe".to_owned()]
    );
    assert_eq!(other.names(), Vec::<String>::new());
}
