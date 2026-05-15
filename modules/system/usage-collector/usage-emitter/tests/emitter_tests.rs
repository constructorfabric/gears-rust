#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    BarrierMode, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};
use chrono::Utc;
use modkit_db::Db;
use modkit_security::pep_properties;
use usage_collector_sdk::models::{AllowedMetric, Subject, UsageKind, UsageRecord};
use usage_collector_sdk::{UsageCollectorClientV1, UsageCollectorError, UsageRecordError};
use usage_emitter::{
    UsageEmitter, UsageEmitterConfig, UsageEmitterError, UsageEmitterFactory, UsageEmitterRuntime,
    UsageEmitterRuntimeV1,
};
use uuid::Uuid;

const TEST_MODULE: &str = "test-module";
const FIXTURE_RESOURCE_TYPE: &str = "test.resource";

// ── Test-specific AuthZ mocks ─────────────────────────────────────────────────

struct AllowOwnerTenantIfInSet {
    extra_allowed: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverClient for AllowOwnerTenantIfInSet {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let subject_tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let Some(subject_tenant) = subject_tenant else {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        };

        let owner = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let Some(owner) = owner else {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        };

        let allowed = owner == subject_tenant || self.extra_allowed.contains(&owner);
        Ok(EvaluationResponse {
            decision: allowed,
            context: EvaluationResponseContext::default(),
        })
    }
}

struct AssertBarrierIgnoreAuthZ;

#[async_trait]
impl AuthZResolverClient for AssertBarrierIgnoreAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let barrier = request
            .context
            .tenant_context
            .as_ref()
            .expect("tenant_context set by AccessRequest::barrier_mode")
            .barrier_mode;
        assert_eq!(barrier, BarrierMode::Ignore);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

struct AssertModulePropertyAuthZ {
    expected_module: String,
    matched: Arc<Mutex<bool>>,
}

#[async_trait]
impl AuthZResolverClient for AssertModulePropertyAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let module_val = request
            .resource
            .properties
            .get("module")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ok = module_val == self.expected_module;
        *self.matched.lock().unwrap() = ok;
        Ok(EvaluationResponse {
            decision: ok,
            context: EvaluationResponseContext::default(),
        })
    }
}

struct AssertSubjectPropertiesAuthZ {
    expected_subject_id: String,
    expected_subject_type: String,
    matched: Arc<Mutex<bool>>,
}

#[async_trait]
impl AuthZResolverClient for AssertSubjectPropertiesAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let subj_id = request
            .resource
            .properties
            .get("subject_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subj_type = request
            .resource
            .properties
            .get("subject_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ok = subj_id == self.expected_subject_id && subj_type == self.expected_subject_type;
        *self.matched.lock().unwrap() = ok;
        Ok(EvaluationResponse {
            decision: ok,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Captures the `SUBJECT_ID` / `SUBJECT_TYPE` resource properties as `Option<String>` so
/// tests can distinguish "property absent" from "property present with some value".
/// Always allows.
struct CapturingSubjectPropertiesAuthZ {
    captured_subject_id: Arc<Mutex<Option<String>>>,
    captured_subject_type: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl AuthZResolverClient for CapturingSubjectPropertiesAuthZ {
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

// ── Test-specific collector mocks ─────────────────────────────────────────────

struct FailingCollector {
    error: fn() -> UsageCollectorError,
}

#[async_trait]
impl UsageCollectorClientV1 for FailingCollector {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<usage_collector_sdk::ModuleConfig, UsageCollectorError> {
        Err((self.error)())
    }
}

/// Collector returning a fixed set of allowed metrics so the merged enqueue-side fixture
/// can exercise gauge/counter metric validation. Used by the `Fixture` struct below.
struct FixtureCollector;

#[async_trait]
impl UsageCollectorClientV1 for FixtureCollector {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<usage_collector_sdk::ModuleConfig, UsageCollectorError> {
        Ok(usage_collector_sdk::ModuleConfig {
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

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn build_factory(name: &str, authz: Arc<dyn AuthZResolverClient>) -> UsageEmitterFactory {
    let db = common::build_db(name).await;
    let runtime = UsageEmitterRuntime::build(
        UsageEmitterConfig::default(),
        db,
        authz,
        Arc::new(common::NoopCollector),
    )
    .await
    .unwrap();
    runtime.factory(TEST_MODULE)
}

async fn build_factory_with_collector(
    name: &str,
    authz: Arc<dyn AuthZResolverClient>,
    collector: Arc<dyn UsageCollectorClientV1>,
) -> UsageEmitterFactory {
    let db = common::build_db(name).await;
    let runtime = UsageEmitterRuntime::build(UsageEmitterConfig::default(), db, authz, collector)
        .await
        .unwrap();
    runtime.factory(TEST_MODULE)
}

// ── Tests (factory-side: PDP authorize behaviour) ─────────────────────────────

#[tokio::test]
async fn build_creates_emitter_with_valid_config() {
    build_factory("emit_build", Arc::new(common::AllowAllAuthZ)).await;
}

#[tokio::test]
async fn authorize_returns_handle_on_allow_all_authz() {
    let factory = build_factory("emit_authz_allow", Arc::new(common::AllowAllAuthZ)).await;
    let ctx = common::make_ctx();
    factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .unwrap();
}

#[tokio::test]
async fn authorize_returns_error_on_deny_all_authz() {
    let factory = build_factory("emit_authz_deny", Arc::new(common::DenyAllAuthZ)).await;
    let ctx = common::make_ctx();
    let Err(err) = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
    else {
        panic!("expected authorization to fail");
    };
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn authorize_returns_permission_denied_on_authz_failure() {
    let factory = build_factory("emit_authz_fail", Arc::new(common::FailingAuthZ)).await;
    let ctx = common::make_ctx();
    let Err(err) = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
    else {
        panic!("expected authorization to fail");
    };
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn authorize_denies_when_pdp_rejects_owner_tenant() {
    let factory = build_factory(
        "emit_pdp_deny_foreign",
        Arc::new(AllowOwnerTenantIfInSet {
            extra_allowed: vec![],
        }),
    )
    .await;

    let subject_tenant = Uuid::new_v4();
    let foreign_tenant = Uuid::new_v4();
    let ctx = modkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(subject_tenant)
        .build()
        .unwrap();

    let Err(err) = factory
        .clone()
        .with_tenant(foreign_tenant)
        .with_subject(Subject::with_type(Uuid::new_v4(), "test.subject"))
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
    else {
        panic!("expected authorization to fail");
    };
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn authorize_allows_subtenant_when_pdp_allows_extra_tenant() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let factory = build_factory(
        "emit_pdp_allow_child",
        Arc::new(AllowOwnerTenantIfInSet {
            extra_allowed: vec![child],
        }),
    )
    .await;

    let ctx = modkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(parent)
        .build()
        .unwrap();

    factory
        .clone()
        .with_tenant(child)
        .with_subject(Subject::with_type(Uuid::new_v4(), "test.subject"))
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .expect("PDP allows emit for allowed sub-tenant");
}

#[tokio::test]
async fn authorize_sends_barrier_mode_ignore_to_pdp() {
    let factory = build_factory("emit_barrier_ignore", Arc::new(AssertBarrierIgnoreAuthZ)).await;
    let ctx = common::make_ctx();
    factory
        .clone()
        .with_tenant(ctx.subject_tenant_id())
        .with_subject(Subject::with_type(Uuid::new_v4(), "test.subject"))
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .expect("barrier assert + allow");
}

#[tokio::test]
async fn authorize_propagates_collector_deadline_exceeded() {
    let factory = build_factory_with_collector(
        "emit_collector_timeout",
        Arc::new(common::AllowAllAuthZ),
        Arc::new(FailingCollector {
            error: || UsageRecordError::deadline_exceeded("plugin timed out").create(),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    let result = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await;
    assert!(matches!(
        result,
        Err(UsageEmitterError::DeadlineExceeded { .. })
    ));
}

#[tokio::test]
async fn authorize_propagates_collector_service_unavailable() {
    let factory = build_factory_with_collector(
        "emit_collector_unavailable",
        Arc::new(common::AllowAllAuthZ),
        Arc::new(FailingCollector {
            error: || UsageEmitterError::service_unavailable().create(),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    let result = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await;
    assert!(matches!(
        result,
        Err(UsageEmitterError::ServiceUnavailable { .. })
    ));
}

#[tokio::test]
async fn authorize_propagates_collector_internal_error() {
    let factory = build_factory_with_collector(
        "emit_collector_internal",
        Arc::new(common::AllowAllAuthZ),
        Arc::new(FailingCollector {
            error: || UsageEmitterError::internal("unexpected state").create(),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    let result = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await;
    assert!(matches!(result, Err(UsageEmitterError::Internal { .. })));
}

#[tokio::test]
async fn authorize_propagates_collector_resource_exhausted() {
    let factory = build_factory_with_collector(
        "emit_collector_resource_exhausted",
        Arc::new(common::AllowAllAuthZ),
        Arc::new(FailingCollector {
            error: || {
                UsageRecordError::resource_exhausted("rate limited")
                    .with_quota_violation("requests", "rate limit exceeded")
                    .create()
            },
        }),
    )
    .await;
    let ctx = common::make_ctx();
    let result = factory
        .clone()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await;
    assert!(matches!(
        result,
        Err(UsageEmitterError::ResourceExhausted { .. })
    ));
}

#[tokio::test]
async fn authorize_pdp_request_includes_module_resource_property() {
    let matched = Arc::new(Mutex::new(false));
    let factory = build_factory(
        "emit_module_prop",
        Arc::new(AssertModulePropertyAuthZ {
            expected_module: TEST_MODULE.to_owned(),
            matched: Arc::clone(&matched),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    drop(
        factory
            .clone()
            .with_tenant(ctx.subject_tenant_id())
            .with_subject(Subject::with_type(Uuid::new_v4(), "test.subject"))
            .authorize(&ctx, Uuid::new_v4(), "test.resource")
            .await,
    );
    assert!(
        *matched.lock().unwrap(),
        "PDP request must include MODULE resource property equal to the module name"
    );
}

#[tokio::test]
async fn authorize_pdp_request_includes_subject_id_and_subject_type_resource_properties() {
    let matched = Arc::new(Mutex::new(false));
    let subject_id = Uuid::new_v4();
    let subject_type = "test.subject.type".to_owned();
    let factory = build_factory(
        "emit_subject_props",
        Arc::new(AssertSubjectPropertiesAuthZ {
            expected_subject_id: subject_id.to_string(),
            expected_subject_type: subject_type.clone(),
            matched: Arc::clone(&matched),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    drop(
        factory
            .clone()
            .with_tenant(ctx.subject_tenant_id())
            .with_subject(Subject::with_type(subject_id, subject_type))
            .authorize(&ctx, Uuid::new_v4(), "test.resource")
            .await,
    );
    assert!(
        *matched.lock().unwrap(),
        "PDP request must include SUBJECT_ID and SUBJECT_TYPE resource properties"
    );
}

#[tokio::test]
async fn authorize_without_subject_omits_subject_properties_from_pdp_request() {
    // `.without_subject()` is the explicit "no subject" intent: PDP receives
    // neither SUBJECT_ID nor SUBJECT_TYPE, regardless of what the
    // SecurityContext carries. This is the contract used by gateways /
    // forwarders to faithfully represent a caller that did not supply a
    // subject — never substituting the gateway's own ctx identity.
    let captured_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_type: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let factory = build_factory(
        "emit_without_subject_pdp_props",
        Arc::new(CapturingSubjectPropertiesAuthZ {
            captured_subject_id: Arc::clone(&captured_id),
            captured_subject_type: Arc::clone(&captured_type),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    factory
        .clone()
        .with_tenant(ctx.subject_tenant_id())
        .without_subject()
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .expect("authorize must succeed when PDP allows and no subject is bound");
    assert!(
        captured_id.lock().unwrap().is_none(),
        "PDP must receive no SUBJECT_ID when `without_subject()` is used"
    );
    assert!(
        captured_type.lock().unwrap().is_none(),
        "PDP must receive no SUBJECT_TYPE when `without_subject()` is used"
    );
    // The downstream wire shape is pinned in the SDK models tests
    // (`usage_record_subject_none_serde`): a `UsageRecord` whose
    // `subject = None` serializes with the `subject` field omitted, so an
    // emitter bound via `.without_subject()` produces an outbox payload
    // that carries no subject — there is no other path from this emitter
    // handle into a record that carries a subject.
}

#[tokio::test]
async fn authorize_default_subject_choice_falls_back_to_ctx_subject_in_pdp() {
    // Neither `.with_subject(...)` nor `.without_subject()` was called —
    // this is the `SubjectChoice::DefaultFromCtx` default for in-process
    // modules whose caller identity *is* the subject. The PDP must receive
    // the ctx-derived SUBJECT_ID. Ctx in this test has no subject type, so
    // SUBJECT_TYPE must be absent.
    let captured_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_type: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let factory = build_factory(
        "emit_default_subject_props_fallback",
        Arc::new(CapturingSubjectPropertiesAuthZ {
            captured_subject_id: Arc::clone(&captured_id),
            captured_subject_type: Arc::clone(&captured_type),
        }),
    )
    .await;
    let ctx = common::make_ctx();
    factory
        .clone()
        .with_tenant(ctx.subject_tenant_id())
        .authorize(&ctx, Uuid::new_v4(), "test.resource")
        .await
        .expect("ctx-derived subject is sent and PDP allows");
    assert_eq!(
        captured_id.lock().unwrap().as_deref(),
        Some(ctx.subject_id().to_string().as_str()),
        "PDP must receive the ctx-derived SUBJECT_ID when neither with_subject nor without_subject was called"
    );
    assert!(
        captured_type.lock().unwrap().is_none(),
        "PDP must omit SUBJECT_TYPE when ctx has no subject type"
    );
}

// ── Enqueue-side fixture (merged from tests/authorized_emitter_tests.rs) ──────

struct Fixture {
    db: Db,
    _runtime: UsageEmitterRuntime,
    emitter: UsageEmitter,
    tenant: Uuid,
    resource_id: Uuid,
}

impl Fixture {
    async fn build(name: &str) -> Self {
        Self::build_with_config(name, UsageEmitterConfig::default()).await
    }

    async fn build_with_config(name: &str, config: UsageEmitterConfig) -> Self {
        let db = common::build_db(name).await;
        let runtime = UsageEmitterRuntime::build(
            config,
            db.clone(),
            Arc::new(common::AllowAllAuthZ),
            Arc::new(FixtureCollector),
        )
        .await
        .unwrap();

        let ctx = common::make_ctx();
        let tenant = ctx.subject_tenant_id();
        let resource_id = Uuid::new_v4();

        let emitter = runtime
            .factory(TEST_MODULE)
            .with_tenant(tenant)
            .with_subject(Subject::with_type(Uuid::nil(), "test.subject"))
            .authorize(&ctx, resource_id, FIXTURE_RESOURCE_TYPE)
            .await
            .unwrap();

        Self {
            db,
            _runtime: runtime,
            emitter,
            tenant,
            resource_id,
        }
    }

    fn record(&self) -> UsageRecord {
        UsageRecord {
            tenant_id: self.tenant,
            module: TEST_MODULE.to_owned(),
            metric: "test.gauge".to_owned(),
            kind: UsageKind::Gauge,
            value: 1.0,
            resource_id: self.resource_id,
            resource_type: FIXTURE_RESOURCE_TYPE.to_owned(),
            subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
            idempotency_key: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            metadata: None,
        }
    }

    fn record_with_kind(&self, kind: UsageKind, value: f64) -> UsageRecord {
        let metric = match kind {
            UsageKind::Gauge => "test.gauge",
            UsageKind::Counter => "test.counter",
        }
        .to_owned();
        UsageRecord {
            metric,
            kind,
            value,
            ..self.record()
        }
    }

    fn conn(&self) -> modkit_db::DbConn<'_> {
        self.db.conn().unwrap()
    }
}

// ── Expiry ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_expired_authorization() {
    let f = Fixture::build_with_config(
        "ap_expired",
        UsageEmitterConfig {
            authorization_max_age: Duration::from_nanos(1),
            ..Default::default()
        },
    )
    .await;
    let err = f
        .emitter
        .enqueue_in(&f.conn(), f.record())
        .await
        .unwrap_err();
    assert!(matches!(err, UsageEmitterError::Unauthenticated { .. }));
}

#[tokio::test]
async fn enqueue_accepts_usage_record() {
    let f = Fixture::build("ap_pos_val").await;
    f.emitter.enqueue_in(&f.conn(), f.record()).await.unwrap();
}

// ── Authorization scope ───────────────────────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_mismatched_tenant() {
    let f = Fixture::build("ap_bad_tenant").await;
    let record = UsageRecord {
        tenant_id: Uuid::new_v4(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_rejects_mismatched_resource_id() {
    let f = Fixture::build("ap_bad_res").await;
    let record = UsageRecord {
        resource_id: Uuid::new_v4(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_rejects_mismatched_resource_type() {
    let f = Fixture::build("ap_bad_rt").await;
    let record = UsageRecord {
        resource_type: "other.resource".to_owned(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

// ── Counter value ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_negative_counter_value() {
    let f = Fixture::build("ap_neg_ctr").await;
    let record = f.record_with_kind(UsageKind::Counter, -1.0);
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}

#[tokio::test]
async fn enqueue_accepts_zero_counter_value() {
    let f = Fixture::build("ap_zero_ctr").await;
    f.emitter
        .enqueue_in(&f.conn(), f.record_with_kind(UsageKind::Counter, 0.0))
        .await
        .unwrap();
}

#[tokio::test]
async fn enqueue_accepts_negative_gauge_value() {
    let f = Fixture::build("ap_neg_gauge").await;
    f.emitter
        .enqueue_in(&f.conn(), f.record_with_kind(UsageKind::Gauge, -1.0))
        .await
        .unwrap();
}

// ── Metric validation ─────────────────────────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_metric_not_in_allowed_list() {
    let f = Fixture::build("ap_metric_disallowed").await;
    let record = UsageRecord {
        metric: "not.allowed.metric".to_owned(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_rejects_metric_kind_mismatch() {
    let f = Fixture::build("ap_kind_mismatch").await;
    let record = UsageRecord {
        metric: "test.counter".to_owned(),
        kind: UsageKind::Gauge,
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}

// ── Module and subject mismatch rejection ─────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_mismatched_module() {
    let f = Fixture::build("ap_bad_module").await;
    let record = UsageRecord {
        module: "wrong-module".to_owned(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_rejects_mismatched_subject_id() {
    let f = Fixture::build("ap_bad_subj_id").await;
    let record = UsageRecord {
        subject: Some(Subject::with_type(Uuid::new_v4(), "test.subject")),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_rejects_mismatched_subject_type() {
    let f = Fixture::build("ap_bad_subj_type").await;
    let record = UsageRecord {
        subject: Some(Subject::with_type(Uuid::nil(), "other.subject")),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::PermissionDenied { .. }));
}

#[tokio::test]
async fn enqueue_accepts_record_when_module_and_subject_match_token() {
    let f = Fixture::build("ap_match_ok").await;
    f.emitter.enqueue_in(&f.conn(), f.record()).await.unwrap();
}

// ── Counter idempotency key ───────────────────────────────────────────────────

#[tokio::test]
async fn enqueue_rejects_counter_record_with_empty_idempotency_key() {
    let f = Fixture::build("ap_ctr_empty_key").await;
    let record = UsageRecord {
        metric: "test.counter".to_owned(),
        kind: UsageKind::Counter,
        idempotency_key: String::new(),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}

// ── Metadata size (default 8192 limit from FixtureCollector) ──────────────────

#[tokio::test]
async fn enqueue_rejects_record_with_oversized_metadata() {
    let f = Fixture::build("ap_metadata_large").await;
    let record = UsageRecord {
        metadata: Some(serde_json::Value::String("x".repeat(8193))),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}
