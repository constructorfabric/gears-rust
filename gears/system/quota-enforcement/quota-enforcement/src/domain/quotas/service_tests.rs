#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    CapPatch, EnforcementMode, IdempotencyScope, IdempotencyWrite, LeaseState, MetricKind,
    NotificationEventKind, OperationType, PayloadHash, PeriodType, QuotaEnforcementStoragePluginV1,
    QuotaId, QuotaSource, QuotaStatus, QuotaType, SubjectRef, TenantId, ValidityWindow,
};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::AccessScope;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::QuotaManagement;
use crate::domain::admission::Admission;
use crate::domain::catalog::MetricCatalog;
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::ports::metrics::{DenialReason, ValidationReason, ValidationSurface};
use crate::domain::quotas::request::{
    CreateQuotaRequest, ListQuotasRequest, Presence, UpdateQuotaRequest,
};
use crate::domain::quotas::validation::{QuotaLimits, RATE_QUOTAS};
use crate::domain::tokens;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, DenyAllPdp, METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS,
    PermitTenantsPdp, RecordingMetrics, TENANT_SUBJECT_TYPE, USER_SUBJECT_TYPE, actor, caller,
    catalog_over, fail_registry_reads, metric_type_document, platform_registry, registry_reads,
    subject_type_document, tenant,
};

fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

fn limits() -> QuotaLimits {
    QuotaLimits {
        metadata_max_bytes: 4096,
        list_max_limit: 500,
        list_max_ids: 100,
        bulk_max_items: 50,
    }
}

struct Harness {
    registry: Arc<MockTypesRegistry>,
    catalog: Arc<MetricCatalog>,
    admission: Admission,
    storage: Arc<InMemoryStorage>,
    metrics: Arc<RecordingMetrics>,
    limits: QuotaLimits,
}

impl Harness {
    async fn new() -> Self {
        Self::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await
    }

    async fn with_pdp(pdp: Arc<dyn AuthZResolverApi>) -> Self {
        let registry = platform_registry();
        let metrics = Arc::new(RecordingMetrics::default());
        let storage = Arc::new(InMemoryStorage::new());
        storage
            .bootstrap(&bundle_with_global_policy())
            .await
            .expect("bootstrap");
        let catalog = catalog_over(&registry, metrics.clone());
        Self {
            registry,
            catalog,
            admission: Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
            storage,
            metrics,
            limits: limits(),
        }
    }

    fn quotas(&self) -> QuotaManagement<'_> {
        QuotaManagement::new(
            self.admission.pdp(caller()),
            &self.catalog,
            self.storage.as_ref(),
            self.metrics.as_ref(),
            self.limits,
        )
    }

    async fn create(&self, request: CreateQuotaRequest) -> QuotaId {
        self.quotas()
            .create(request)
            .await
            .expect("create")
            .quota
            .id
    }

    fn seed_unavailable(&self, metric: &str) {
        let mut broken = metric_type_document(metric, json!({ "kind": "counter" }), json!({}));
        broken["x-gts-abstract"] = json!(true);
        self.registry.seed(metric, broken);
    }
}

fn user_subject(id: &str) -> SubjectRef {
    SubjectRef {
        subject_type: type_id(USER_SUBJECT_TYPE),
        subject_id: id.to_owned(),
    }
}

fn metadata(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().expect("object")
}

fn request() -> CreateQuotaRequest {
    CreateQuotaRequest {
        tenant_id: tenant(),
        subject: user_subject("u1"),
        metric: METRIC_TOKENS.to_owned(),
        quota_type: QuotaType::Consumption,
        period: Presence::Value(PeriodType::Month),
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(100),
        notification_thresholds: vec![50],
        validity_window: None,
        fail_open_hint: false,
        metadata: None,
        source: QuotaSource::Operator,
    }
}

fn idem(op: OperationType, key: &str) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            operation_type: op,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([1; 32]),
    }
}

// A counter mutation through the shared convention: the transaction selects
// the policy and evaluates, so these tests state an amount, not a plan.
async fn debit(storage: &InMemoryStorage, amount: u64, write: &IdempotencyWrite) {
    let evaluator = std::sync::Arc::new(quota_enforcement_sdk::testing::ScriptedEvaluator::new());
    let call: std::sync::Arc<quota_enforcement_sdk::engine::TransactionEvaluator> =
        std::sync::Arc::new(
            move |context: &quota_enforcement_sdk::EvaluationContext<'_>| {
                evaluator.evaluate(context)
            },
        );
    storage
        .apply_debit_plan(&actor(), &scope(), &evaluated(amount, write, call), &[])
        .await
        .expect("debit");
}

fn evaluated(
    amount: u64,
    write: &IdempotencyWrite,
    call: std::sync::Arc<quota_enforcement_sdk::engine::TransactionEvaluator>,
) -> quota_enforcement_sdk::EvaluatedMutation<'_> {
    quota_enforcement_sdk::EvaluatedMutation {
        applicable: applicable(),
        amount,
        request: &serde_json::Value::Null,
        metric_id: None,
        limits: test_limits_clamp(),
        idempotency: write,
        authorized: quota_enforcement_sdk::AttributionDigest::from_bytes([7; 32]),
        evaluate: call,
    }
}

fn test_limits_clamp() -> quota_enforcement_sdk::engine::EvaluationLimits {
    quota_enforcement_sdk::engine::EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(5).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

fn applicable() -> &'static quota_enforcement_sdk::ApplicableQuotas {
    static APPLICABLE: std::sync::OnceLock<quota_enforcement_sdk::ApplicableQuotas> =
        std::sync::OnceLock::new();
    APPLICABLE.get_or_init(|| quota_enforcement_sdk::ApplicableQuotas {
        tenant_id: tenant(),
        subjects: vec![user_subject("u1")],
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
    })
}

fn scope() -> AccessScope {
    AccessScope::for_tenant(tenant().as_uuid())
}

// --- create ------------------------------------------------------------------

#[tokio::test]
async fn ac1_create_persists_an_active_row_and_enqueues_the_created_event() {
    let h = Harness::new().await;
    let view = h.quotas().create(request()).await.expect("create");
    assert_eq!(view.quota.status, QuotaStatus::Active);
    assert_eq!(view.quota.record_version, 1);
    assert_eq!(view.metric_kind, Some(MetricKind::Counter));
    assert!(view.currently_within_window);
    let stored = h.storage.quota(view.quota.id).expect("stored");
    assert_eq!(stored.metadata, view.quota.metadata);
    let events = h.storage.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, NotificationEventKind::QuotaChanged);
    assert_eq!(events[0].quota_id, Some(view.quota.id));
    assert_eq!(events[0].payload, json!({ "change_kind": "created" }));
    let reads = registry_reads(&h.registry);
    h.create(request()).await;
    assert_eq!(
        registry_reads(&h.registry),
        reads,
        "a held metric and a cached subject type need no registry read"
    );
}

#[tokio::test]
async fn ac2_an_unregistered_metric_and_an_unreachable_registry_fail_closed_before_storage() {
    let h = Harness::new().await;
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            metric: METRIC_OTHER.to_owned(),
            ..request()
        })
        .await
        .expect_err("unknown metric");
    assert_eq!(
        err,
        DomainError::MetricNotRegistered {
            metric: METRIC_OTHER.to_owned()
        }
    );
    assert!(matches!(
        CanonicalError::from(err),
        CanonicalError::InvalidArgument { .. }
    ));
    fail_registry_reads(&h.registry);
    let err = h
        .quotas()
        .create(request())
        .await
        .expect_err("registry down on first use");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
    assert!(matches!(
        CanonicalError::from(err),
        CanonicalError::ServiceUnavailable { .. }
    ));
    assert!(h.storage.events().is_empty(), "nothing persisted");
}

#[tokio::test]
async fn a_quota_write_for_a_metric_with_no_served_entry_is_not_resolvable() {
    let h = Harness::new().await;
    let id = h.create(request()).await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.seed_unavailable(broken);
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            metric: broken.to_owned(),
            ..request()
        })
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
    let CanonicalError::FailedPrecondition { ctx: violation, .. } = CanonicalError::from(err)
    else {
        panic!("FailedPrecondition expected");
    };
    assert_eq!(
        violation.violations[0].type_,
        DomainError::PROJECTION_NOT_RESOLVABLE
    );
    assert!(
        h.storage.events().len() == 1,
        "only the first create persisted"
    );
    h.quotas().deactivate(id).await.expect("deactivate runs");
}

#[tokio::test]
async fn ac3_subject_admission_and_scope_are_enforced_before_persistence() {
    let h = Harness::new().await;
    let derived = "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~acme.core.quota.admin.v1~";
    h.registry.seed(derived, subject_type_document(derived));
    h.create(CreateQuotaRequest {
        subject: SubjectRef {
            subject_type: type_id(derived),
            subject_id: "a1".to_owned(),
        },
        ..request()
    })
    .await;

    let device = CreateQuotaRequest {
        subject: SubjectRef {
            subject_type: type_id(DEVICE_SUBJECT_TYPE),
            subject_id: "d1".to_owned(),
        },
        ..request()
    };
    let err = h
        .quotas()
        .create(device.clone())
        .await
        .expect_err("the metric does not admit devices");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "subject.type",
            reason: tokens::METRIC_NOT_ADMITTED
        }
    );
    assert_eq!(
        h.metrics.admitted_violations(),
        vec![ValidationSurface::QuotaWrite]
    );
    h.quotas()
        .create(CreateQuotaRequest {
            metric: METRIC_REQUESTS.to_owned(),
            ..device
        })
        .await
        .expect("a metric admitting the subject base admits devices");

    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            subject: SubjectRef {
                subject_type: type_id("gts.cf.core.quota.subj.v1~acme.core.quota.robot.v1~"),
                subject_id: "r1".to_owned(),
            },
            metric: METRIC_REQUESTS.to_owned(),
            ..request()
        })
        .await
        .expect_err("unregistered subject type");
    assert!(
        matches!(err, DomainError::ProjectionNotRegistered { .. }),
        "{err:?}"
    );

    let tenant_subject = |id: String| CreateQuotaRequest {
        subject: SubjectRef {
            subject_type: type_id(TENANT_SUBJECT_TYPE),
            subject_id: id,
        },
        ..request()
    };
    let err = h
        .quotas()
        .create(tenant_subject("someone-else".to_owned()))
        .await
        .expect_err("a tenant subject names the Quota's tenant");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "subject.id",
            reason: tokens::TENANT_SUBJECT_MISMATCH
        }
    );
    h.quotas()
        .create(tenant_subject(tenant().to_string()))
        .await
        .expect("the tenant itself, always admitted");
    assert_eq!(
        h.storage.events().len(),
        3,
        "only the accepted creates persisted"
    );
}

#[tokio::test]
async fn ac4_negative_caps_fail_before_the_pdp_while_zero_and_unbounded_are_counted() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            cap: Some(-1),
            ..request()
        })
        .await
        .expect_err("negative");
    assert_eq!(err, DomainError::CapMustBeNonNegative { cap: -1 });
    assert_eq!(pdp.calls(), 0, "shape fails before the PDP");
    assert_eq!(h.metrics.denials(), vec![DenialReason::InvalidArgument]);

    h.create(CreateQuotaRequest {
        cap: Some(0),
        ..request()
    })
    .await;
    h.create(CreateQuotaRequest {
        cap: None,
        notification_thresholds: Vec::new(),
        ..request()
    })
    .await;
    let counts = h.storage.read_active_quota_counts().await.expect("counts");
    assert_eq!((counts.cap_zero, counts.cap_unbounded), (1, 1));
}

#[tokio::test]
async fn ac5_thresholds_require_a_bounded_cap_at_create_and_at_update() {
    let h = Harness::new().await;
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            cap: None,
            ..request()
        })
        .await
        .expect_err("thresholds on unbounded");
    assert_eq!(err, DomainError::ThresholdsRequireBoundedCap);

    let id = h.create(request()).await;
    let err = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                cap: Presence::Null,
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("unbinding a row that keeps thresholds");
    assert_eq!(err, DomainError::ThresholdsRequireBoundedCap);
    let unbounded = h
        .create(CreateQuotaRequest {
            cap: None,
            notification_thresholds: Vec::new(),
            ..request()
        })
        .await;
    let err = h
        .quotas()
        .update(
            unbounded,
            UpdateQuotaRequest {
                notification_thresholds: Some(vec![50]),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("thresholds on an unbounded row");
    assert_eq!(err, DomainError::ThresholdsRequireBoundedCap);
}

#[tokio::test]
async fn ac6_several_quotas_per_subject_and_metric_are_accepted() {
    let h = Harness::new().await;
    let first = h.create(request()).await;
    let second = h.create(request()).await;
    assert_ne!(first, second);
}

#[tokio::test]
async fn ac7_rate_is_unimplemented_on_create_and_update_before_any_other_gate() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            quota_type: QuotaType::Rate,
            period: Presence::Absent,
            ..request()
        })
        .await
        .expect_err("rate create");
    assert_eq!(
        err,
        DomainError::NotYetImplemented {
            feature: RATE_QUOTAS
        }
    );
    let id = h.create(request()).await;
    let err = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                metric: Presence::Value(json!(METRIC_OTHER)),
                quota_type: Presence::Value(json!(QuotaType::Rate.as_gts_id())),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("rate update");
    assert_eq!(
        err,
        DomainError::NotYetImplemented {
            feature: RATE_QUOTAS
        }
    );
    assert_eq!(pdp.calls(), 1, "only the accepted create reached the PDP");
    assert!(
        h.metrics.denials().is_empty(),
        "a reserved capability is not an invalid-argument denial"
    );
}

#[tokio::test]
async fn ac8_the_cap_guard_is_decided_by_storage_and_raises_bypass_it() {
    let h = Harness::new().await;
    let id = h.create(request()).await;
    debit(&h.storage, 60, &idem(OperationType::Debit, "d")).await;
    let err = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                cap: Presence::Value(50),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("below consumed");
    assert_eq!(
        err,
        DomainError::CapBelowConsumed {
            new_cap: 50,
            consumed: 60
        }
    );
    let raised = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                cap: Presence::Value(200),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect("a raise bypasses the guard");
    assert_eq!(raised.quota.cap, Some(200));
    assert_eq!(raised.quota.record_version, 2);
    let unbound = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                cap: Presence::Null,
                notification_thresholds: Some(Vec::new()),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect("numeric to unbounded bypasses the guard");
    assert_eq!(unbound.quota.cap, None);
}

#[tokio::test]
async fn ac9_immutable_fields_are_rejected_and_identity_survives_updates() {
    let h = Harness::new().await;
    let id = h.create(request()).await;
    for (field, patch) in [
        (
            "metric",
            UpdateQuotaRequest {
                metric: Presence::Null,
                ..UpdateQuotaRequest::default()
            },
        ),
        (
            "period",
            UpdateQuotaRequest {
                period: Presence::Value(json!("gts.cf.core.qe.period_type.v1~cf.core.qe.day.v1")),
                ..UpdateQuotaRequest::default()
            },
        ),
        (
            "subject",
            UpdateQuotaRequest {
                subject: Presence::Value(json!({ "id": "u2" })),
                ..UpdateQuotaRequest::default()
            },
        ),
    ] {
        let err = h.quotas().update(id, patch).await.expect_err(field);
        assert_eq!(
            err,
            DomainError::InvalidArgument {
                field,
                reason: tokens::IMMUTABLE_FIELD
            }
        );
    }
    let updated = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                fail_open_hint: Some(true),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect("accepted");
    assert_eq!(updated.quota.id, id);
    assert_eq!(updated.quota.subject, user_subject("u1"));
    assert!(updated.quota.fail_open_hint);
    let events = h.storage.events();
    assert_eq!(
        events.last().map(|e| e.payload.clone()),
        Some(json!({ "change_kind": "updated" }))
    );
}

#[tokio::test]
async fn ac10_metadata_is_bounded_and_checked_against_the_metric_type_before_persistence() {
    let h = Harness::new().await;
    let small = Harness {
        limits: QuotaLimits {
            metadata_max_bytes: 4,
            ..limits()
        },
        ..Harness::new().await
    };
    let err = small
        .quotas()
        .create(CreateQuotaRequest {
            metadata: Some(metadata(&json!({ "region": "eu" }))),
            ..request()
        })
        .await
        .expect_err("oversize");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "metadata",
            reason: tokens::METADATA_TOO_LARGE
        }
    );
    let over_4kb = Map::from_iter([("blob".to_owned(), json!("x".repeat(4096)))]);
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            metadata: Some(over_4kb),
            ..request()
        })
        .await
        .expect_err("over 4 KB");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "metadata",
            reason: tokens::METADATA_TOO_LARGE
        }
    );

    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            metadata: Some(metadata(&json!({ "bad": true, "region": 7 }))),
            ..request()
        })
        .await
        .expect_err("conditions that do not fit");
    let DomainError::QuotaMetadataRejected { violations } = &err else {
        panic!("rejection expected: {err:?}");
    };
    assert_eq!(violations.len(), 2);
    let CanonicalError::InvalidArgument { ctx: invalid, .. } = CanonicalError::from(err.clone())
    else {
        panic!("InvalidArgument expected");
    };
    let toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations } = invalid
    else {
        panic!("field violations expected");
    };
    let fields: Vec<(&str, &str)> = field_violations
        .iter()
        .map(|v| (v.field.as_str(), v.reason.as_str()))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("metadata/bad", "QUOTA_METADATA_REJECTED"),
            ("metadata/region", "QUOTA_METADATA_REJECTED"),
        ]
    );
    assert_eq!(
        h.metrics.contract_failures(),
        vec![(
            ValidationSurface::QuotaWrite,
            ValidationReason::SchemaViolation
        )]
    );
    assert!(h.storage.events().is_empty(), "rejected before persistence");

    let id = h.create(request()).await;
    let updated = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                metadata: Some(metadata(&json!({ "region": ["eu"], "model": "m-9" }))),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect("metadata-only update whose conditions fit");
    assert_eq!(updated.quota.id, id);
    assert_eq!(updated.quota.metadata["model"], json!("m-9"));
    let err = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                metadata: Some(metadata(&json!({ "bad": 1 }))),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("a metadata change is checked again");
    assert!(matches!(err, DomainError::QuotaMetadataRejected { .. }));
    assert_eq!(
        h.storage.events().len(),
        2,
        "create and the accepted update each enqueued one event"
    );
}

#[tokio::test]
async fn a_quota_update_for_a_metric_that_became_unavailable_is_refused() {
    let h = Harness::new().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.later.v1~";
    let first = Harness::new().await;
    first.registry.seed(
        broken,
        metric_type_document(
            broken,
            json!({ "kind": "counter", "admitted_subjects": [USER_SUBJECT_TYPE] }),
            json!({}),
        ),
    );
    let id = first
        .create(CreateQuotaRequest {
            metric: broken.to_owned(),
            ..request()
        })
        .await;
    let later = Harness {
        storage: first.storage.clone(),
        ..h
    };
    later.seed_unavailable(broken);
    let err = later
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                fail_open_hint: Some(true),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("no served entry");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
    later
        .quotas()
        .deactivate(id)
        .await
        .expect("a deactivation makes no catalogue check");
}

#[tokio::test]
async fn ac11_deactivation_resolves_active_leases_once_and_is_terminal() {
    let h = Harness::new().await;
    let id = h.create(request()).await;
    let write = idem(OperationType::Reserve, "r");
    let evaluator = std::sync::Arc::new(quota_enforcement_sdk::testing::ScriptedEvaluator::new());
    let call: std::sync::Arc<quota_enforcement_sdk::engine::TransactionEvaluator> =
        std::sync::Arc::new(
            move |context: &quota_enforcement_sdk::EvaluationContext<'_>| {
                evaluator.evaluate(context)
            },
        );
    let acquired = h
        .storage
        .acquire_lease(
            &actor(),
            &scope(),
            &evaluated(60, &write, call),
            std::time::Duration::from_mins(1),
        )
        .await
        .expect("lease");
    let token = acquired.get().token.expect("an allowed acquisition holds");
    let outcome = h.quotas().deactivate(id).await.expect("deactivate");
    assert_eq!(outcome.resolved_leases, vec![token]);
    assert_eq!(
        h.storage.lease_state(token),
        Some(LeaseState::ResolvedByDeactivation)
    );
    assert_eq!(
        h.storage.events().last().map(|e| e.payload.clone()),
        Some(json!({ "change_kind": "deactivated" }))
    );
    let again = h.quotas().deactivate(id).await.expect_err("terminal");
    assert_eq!(again, DomainError::QuotaDeactivated { id: id.to_string() });
    let patched = h
        .quotas()
        .update(
            id,
            UpdateQuotaRequest {
                fail_open_hint: Some(true),
                ..UpdateQuotaRequest::default()
            },
        )
        .await
        .expect_err("no patch after deactivation");
    assert_eq!(
        patched,
        DomainError::QuotaDeactivated { id: id.to_string() }
    );
}

#[tokio::test]
async fn ac12_deactivated_quotas_stay_readable_and_reads_are_pdp_gated() {
    let h = Harness::new().await;
    let id = h
        .create(CreateQuotaRequest {
            metadata: Some(metadata(&json!({ "region": ["eu"], "model": "m-5" }))),
            ..request()
        })
        .await;
    h.quotas().deactivate(id).await.expect("deactivate");
    let one = h.quotas().get(id).await.expect("get");
    assert_eq!(one.quota.status, QuotaStatus::Deactivated);
    assert_eq!(one.quota.metadata["model"], json!("m-5"));
    assert_eq!(one.metric_kind, Some(MetricKind::Counter));
    let page = h
        .quotas()
        .list(ListQuotasRequest::default())
        .await
        .expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].quota.id, id);
    let missing = h
        .quotas()
        .get(QuotaId::new(Uuid::from_u128(404)))
        .await
        .expect_err("unknown");
    assert_eq!(
        missing,
        DomainError::NotFound {
            kind: ResourceKind::Quota,
            id: QuotaId::new(Uuid::from_u128(404)).to_string()
        }
    );

    let denied = Harness::with_pdp(Arc::new(DenyAllPdp)).await;
    let err = denied
        .quotas()
        .list(ListQuotasRequest::default())
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::PdpDenied { .. }), "{err:?}");
    let err = denied
        .quotas()
        .create(request())
        .await
        .expect_err("denied create");
    assert!(matches!(err, DomainError::PdpDenied { .. }), "{err:?}");
    assert_eq!(
        registry_reads(&denied.registry),
        0,
        "the PDP answers before any registry call"
    );
    assert!(denied.storage.events().is_empty());
}

#[tokio::test]
async fn a_list_without_a_tenant_returns_only_the_callers_own_tenant() {
    let other = TenantId::new(Uuid::from_u128(0x0e7e_0000_0000_0000_0000_0000_0000_0002));
    let h = Harness::with_pdp(Arc::new(PermitTenantsPdp::new(vec![
        tenant().as_uuid(),
        other.as_uuid(),
    ])))
    .await;
    let own = h.create(request()).await;
    h.create(CreateQuotaRequest {
        tenant_id: other,
        ..request()
    })
    .await;
    let page = h
        .quotas()
        .list(ListQuotasRequest::default())
        .await
        .expect("list");
    let ids: Vec<QuotaId> = page.items.iter().map(|v| v.quota.id).collect();
    assert_eq!(
        ids,
        vec![own],
        "an absent tenant_id means the caller's own tenant"
    );
}

#[tokio::test]
async fn ac13_a_quota_on_a_direct_metric_is_accepted() {
    let h = Harness::new().await;
    let direct = "gts.cf.core.quota.metric.v1~cf.qetest.metric.direct.v1~";
    h.registry.seed(
        direct,
        metric_type_document(
            direct,
            json!({
                "kind": "counter",
                "enforcement": "direct",
                "admitted_subjects": [USER_SUBJECT_TYPE],
            }),
            json!({}),
        ),
    );
    let view = h
        .quotas()
        .create(CreateQuotaRequest {
            metric: direct.to_owned(),
            ..request()
        })
        .await
        .expect("accepted");
    assert_eq!(view.quota.status, QuotaStatus::Active);
}

#[tokio::test]
async fn ac14_a_past_validity_end_flips_the_flag_and_never_the_status() {
    let h = Harness::new().await;
    let past = OffsetDateTime::from_unix_timestamp(1_000).expect("timestamp");
    let id = h
        .create(CreateQuotaRequest {
            validity_window: Some(ValidityWindow {
                start: None,
                end: Some(past),
            }),
            ..request()
        })
        .await;
    let view = h.quotas().get(id).await.expect("get");
    assert_eq!(view.quota.status, QuotaStatus::Active);
    assert!(!view.currently_within_window);
}

// --- precedence and the read path ---------------------------------------------

#[tokio::test]
async fn a_shape_failure_wins_over_a_denying_pdp_and_a_denial_over_the_registry() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp)).await;
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            subject: user_subject(""),
            ..request()
        })
        .await
        .expect_err("shape");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "subject.id",
            reason: tokens::SUBJECT_ID_REQUIRED
        }
    );
    let err = h
        .quotas()
        .create(CreateQuotaRequest {
            metric: METRIC_OTHER.to_owned(),
            ..request()
        })
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::PdpDenied { .. }));
    assert_eq!(registry_reads(&h.registry), 0);
}

#[tokio::test]
async fn reads_tolerate_a_registry_outage_and_a_metric_without_a_served_entry() {
    let first = Harness::new().await;
    let id = first.create(request()).await;

    let cold = Harness {
        storage: first.storage.clone(),
        ..Harness::new().await
    };
    fail_registry_reads(&cold.registry);
    let view = cold
        .quotas()
        .get(id)
        .await
        .expect("readable with the registry down");
    assert_eq!(view.metric_kind, None, "unknown, never defaulted");

    let broken = Harness {
        storage: first.storage.clone(),
        ..Harness::new().await
    };
    broken.seed_unavailable(METRIC_TOKENS);
    let view = broken.quotas().get(id).await.expect("still readable");
    assert_eq!(view.metric_kind, None, "unknown, never defaulted");
    let page = broken
        .quotas()
        .list(ListQuotasRequest::default())
        .await
        .expect("list");
    assert_eq!(page.items[0].metric_kind, None);

    let warm = Harness {
        storage: first.storage.clone(),
        ..Harness::new().await
    };
    warm.quotas().get(id).await.expect("holds the tokens type");
    first
        .create(CreateQuotaRequest {
            metric: METRIC_REQUESTS.to_owned(),
            ..request()
        })
        .await;
    fail_registry_reads(&warm.registry);
    let page = warm
        .quotas()
        .list(ListQuotasRequest::default())
        .await
        .expect("list");
    let kinds: Vec<_> = page
        .items
        .iter()
        .map(|view| (view.quota.id == id, view.metric_kind))
        .collect();
    assert!(
        kinds.contains(&(true, Some(MetricKind::Counter))),
        "a held metric keeps its kind: {kinds:?}"
    );
    assert!(kinds.contains(&(false, None)), "{kinds:?}");
}

#[tokio::test]
async fn the_in_process_conversions_keep_caps_in_range() {
    let h = Harness::new().await;
    let spec = quota_enforcement_sdk::QuotaSpec {
        tenant_id: tenant(),
        subject: user_subject("u1"),
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
        quota_type: QuotaType::Consumption,
        period: Some(PeriodType::Month),
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(u64::MAX),
        notification_thresholds: Vec::new(),
        validity_window: None,
        fail_open_hint: false,
        metadata: serde_json::Map::new(),
        source: QuotaSource::Operator,
    };
    let err = CreateQuotaRequest::try_from(spec.clone()).expect_err("above i64::MAX");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "cap",
            reason: tokens::CAP_OUT_OF_RANGE
        }
    );
    let max = CreateQuotaRequest::try_from(quota_enforcement_sdk::QuotaSpec {
        cap: Some(quota_enforcement_sdk::Quota::MAX_CAP),
        metadata: json!({ "region": ["eu"], "model": "m-5" })
            .as_object()
            .cloned()
            .expect("object"),
        ..spec
    })
    .expect("the largest supported cap");
    let view = h.quotas().create(max).await.expect("accepted end to end");
    assert_eq!(view.quota.cap, Some(quota_enforcement_sdk::Quota::MAX_CAP));
    let patch = quota_enforcement_sdk::QuotaUpdate {
        cap: Some(CapPatch::Bounded(u64::MAX)),
        ..quota_enforcement_sdk::QuotaUpdate::default()
    };
    let err = UpdateQuotaRequest::try_from(patch).expect_err("above i64::MAX");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "cap",
            reason: tokens::CAP_OUT_OF_RANGE
        }
    );
}

#[tokio::test]
async fn a_quota_type_the_kind_of_its_metric_does_not_take_is_invalid() {
    let h = Harness::new().await;
    let gauge = "gts.cf.core.quota.metric.v1~cf.qetest.metric.seats.v1~";
    h.registry.seed(
        gauge,
        metric_type_document(
            gauge,
            json!({ "kind": "gauge", "admitted_subjects": [USER_SUBJECT_TYPE] }),
            json!({}),
        ),
    );
    let allocation = |metric: &str| CreateQuotaRequest {
        metric: metric.to_owned(),
        quota_type: QuotaType::Allocation,
        period: Presence::Absent,
        metadata: None,
        ..request()
    };
    let consumption_on_gauge = CreateQuotaRequest {
        metric: gauge.to_owned(),
        metadata: None,
        ..request()
    };

    for refused in [allocation(METRIC_TOKENS), consumption_on_gauge] {
        let err = h
            .quotas()
            .create(refused)
            .await
            .expect_err("the kind does not take the quota type");
        assert!(
            matches!(
                err,
                DomainError::InvalidArgument {
                    field: "quota_type",
                    reason
                } if reason == tokens::QUOTA_TYPE_KIND_MISMATCH
            ),
            "{err:?}"
        );
        assert!(matches!(
            CanonicalError::from(err),
            CanonicalError::InvalidArgument { .. }
        ));
    }
    assert!(h.storage.events().is_empty(), "nothing persisted");

    h.create(allocation(gauge)).await;
    h.create(CreateQuotaRequest {
        metadata: None,
        ..request()
    })
    .await;
}

#[tokio::test]
async fn with_the_registry_down_a_create_checks_admission_and_the_metric_before_the_subject_type() {
    let h = Harness::new().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.seed_unavailable(broken);
    for metric in [METRIC_TOKENS, broken] {
        h.catalog
            .served(
                &quota_enforcement_sdk::MetricId::parse(metric).expect("metric"),
                ValidationSurface::QuotaWrite,
            )
            .await
            .ok();
    }
    fail_registry_reads(&h.registry);
    let device = |metric: &str| CreateQuotaRequest {
        subject: SubjectRef {
            subject_type: type_id(DEVICE_SUBJECT_TYPE),
            subject_id: "d1".to_owned(),
        },
        metric: metric.to_owned(),
        ..request()
    };

    let err = h
        .quotas()
        .create(device(METRIC_TOKENS))
        .await
        .expect_err("the metric does not admit devices");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "subject.type",
            reason: tokens::METRIC_NOT_ADMITTED
        }
    );
    let err = h
        .quotas()
        .create(device(broken))
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_create_on_a_held_metric_reads_its_subject_type_with_one_read() {
    let h = Harness::new().await;
    h.catalog
        .served(
            &quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
            ValidationSurface::QuotaWrite,
        )
        .await
        .expect("held");
    let reads = registry_reads(&h.registry);

    h.create(request()).await;

    assert_eq!(registry_reads(&h.registry), reads + 1);
}
