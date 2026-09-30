#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    DenyReason, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    EnforcementMode, PeriodType, QuotaId, QuotaSource, QuotaStatus, QuotaType, SubjectRef,
};
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

use super::{
    BULK_MAX_ITEMS_CEILING, BulkCreateItem, BulkCreateRequest, BulkDeactivateItem,
    BulkDeactivateRequest, BulkUpdateItem, BulkUpdateRequest,
};
use crate::domain::admission::Admission;
use crate::domain::catalog::{CatalogBuilder, CatalogConfig, ProjectionContractCatalog};
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::quotas::QuotaManagement;
use crate::domain::quotas::request::{CreateQuotaRequest, Presence, UpdateQuotaRequest};
use crate::domain::quotas::validation::QuotaLimits;
use crate::domain::tokens;
use crate::test_support::{
    DenyAllPdp, FakeContractRegistry, FakeMetricRegistry, LLM_TENANT_PROJECTION,
    LLM_USER_PROJECTION, METRIC_TOKENS, PermitTenantsPdp, RecordingMetrics, ctx, tenant,
};

fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

fn limits(bulk_max_items: usize) -> QuotaLimits {
    QuotaLimits {
        metadata_max_bytes: 4096,
        list_max_limit: 500,
        list_max_ids: 100,
        bulk_max_items,
    }
}

/// Permits every call like [`PermitTenantsPdp`], except one resource id.
struct DenyOnePdp {
    denied: Uuid,
    calls: AtomicUsize,
}

#[async_trait]
impl AuthZResolverApi for DenyOnePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.resource.id == Some(self.denied) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(DenyReason {
                        error_code: "NOT_THIS_ONE".to_owned(),
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
                        vec![tenant().as_uuid()],
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

struct Harness {
    catalog: ProjectionContractCatalog,
    admission: Admission,
    storage: Arc<InMemoryStorage>,
    registry: Arc<FakeContractRegistry>,
    metric_registry: Arc<FakeMetricRegistry>,
    metrics: Arc<RecordingMetrics>,
    limits: QuotaLimits,
}

impl Harness {
    async fn new(pdp: Arc<dyn AuthZResolverApi>) -> Self {
        Self::over(pdp, Arc::new(InMemoryStorage::new()), limits(50)).await
    }

    async fn over(
        pdp: Arc<dyn AuthZResolverApi>,
        storage: Arc<InMemoryStorage>,
        limits: QuotaLimits,
    ) -> Self {
        let metrics = Arc::new(RecordingMetrics::default());
        let registry = Arc::new(FakeContractRegistry::llm_gateway());
        let catalog = CatalogBuilder::new(registry.as_ref(), metrics.as_ref())
            .build(&CatalogConfig {
                subject_projections: vec![
                    type_id(LLM_USER_PROJECTION),
                    type_id(LLM_TENANT_PROJECTION),
                ],
                resource_projections: Vec::new(),
            })
            .await
            .expect("catalogue");
        Self {
            catalog,
            admission: Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
            storage,
            registry,
            metric_registry: Arc::new(FakeMetricRegistry::classified()),
            metrics,
            limits,
        }
    }

    fn quotas(&self) -> QuotaManagement<'_> {
        QuotaManagement::new(
            &self.admission,
            &self.catalog,
            self.storage.as_ref(),
            self.registry.as_ref(),
            self.metric_registry.as_ref(),
            self.metrics.as_ref(),
            self.limits,
        )
    }

    async fn create_one(&self, subject: &str) -> QuotaId {
        self.quotas()
            .create(&ctx(), draft(subject))
            .await
            .expect("create")
            .quota
            .id
    }
}

/// A valid consumption draft on the user projection.
fn draft(subject: &str) -> CreateQuotaRequest {
    CreateQuotaRequest {
        tenant_id: tenant(),
        subject: SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: subject.to_owned(),
        },
        metric: METRIC_TOKENS.to_owned(),
        quota_type: QuotaType::Consumption,
        period: Presence::Value(PeriodType::Month),
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(100),
        notification_thresholds: vec![50],
        validity_window: None,
        fail_open_hint: false,
        metadata: Some(
            json!({ "regions": ["eu"], "weight": 5 })
                .as_object()
                .cloned()
                .expect("object"),
        ),
        source: QuotaSource::Operator,
    }
}

fn creates(key: &str, drafts: Vec<CreateQuotaRequest>) -> BulkCreateRequest {
    BulkCreateRequest {
        tenant_id: tenant(),
        idempotency_key: key.to_owned(),
        items: drafts
            .into_iter()
            .map(|request| BulkCreateItem {
                idempotency_key: None,
                request: Ok(request),
            })
            .collect(),
    }
}

fn cap_patch(cap: i64) -> UpdateQuotaRequest {
    UpdateQuotaRequest {
        cap: Presence::Value(cap),
        ..UpdateQuotaRequest::default()
    }
}

fn updates(key: &str, items: Vec<(QuotaId, UpdateQuotaRequest)>) -> BulkUpdateRequest {
    BulkUpdateRequest {
        tenant_id: tenant(),
        idempotency_key: key.to_owned(),
        items: items
            .into_iter()
            .map(|(quota_id, request)| BulkUpdateItem {
                idempotency_key: None,
                quota_id,
                request: Ok(request),
            })
            .collect(),
    }
}

fn deactivations(key: &str, ids: &[QuotaId]) -> BulkDeactivateRequest {
    BulkDeactivateRequest {
        tenant_id: tenant(),
        idempotency_key: key.to_owned(),
        items: ids
            .iter()
            .map(|quota_id| BulkDeactivateItem {
                idempotency_key: None,
                quota_id: *quota_id,
            })
            .collect(),
    }
}

fn item_error(error: &DomainError) -> (usize, &DomainError) {
    match error {
        DomainError::BulkItem { index, cause } => (*index, cause.as_ref()),
        other => panic!("expected an item error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_envelope_over_the_ceiling_is_refused_before_any_pdp_call() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::new(pdp.clone()).await;
    let ids: Vec<QuotaId> = (0..=BULK_MAX_ITEMS_CEILING)
        .map(|_| QuotaId::generate())
        .collect();
    let err = h
        .quotas()
        .bulk_deactivate(&ctx(), deactivations("off", &ids))
        .await
        .expect_err("too large");
    assert_eq!(
        err,
        DomainError::BulkTooLarge {
            items: BULK_MAX_ITEMS_CEILING + 1,
            max: BULK_MAX_ITEMS_CEILING
        }
    );
    assert_eq!(
        pdp.calls(),
        0,
        "no authorization work for an oversized envelope"
    );
}

#[tokio::test]
async fn a_bulk_create_commits_every_draft_and_replays_after_the_limit_is_lowered() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::new(pdp.clone()).await;
    let request = creates("pack", vec![draft("u1"), draft("u2")]);
    let created = h
        .quotas()
        .bulk_create(&ctx(), request.clone())
        .await
        .expect("bulk create");
    assert_eq!(created.items.len(), 2);
    assert_eq!(pdp.calls(), 1, "one tenant-level check for the envelope");
    for item in &created.items {
        let quota = h.storage.quota(item.quota_id).expect("stored");
        assert_eq!(quota.status, QuotaStatus::Active);
    }

    let lowered = Harness::over(pdp, Arc::clone(&h.storage), limits(1)).await;
    let replay = lowered
        .quotas()
        .bulk_create(&ctx(), request)
        .await
        .expect("a committed envelope replays past a lowered limit");
    assert_eq!(replay, created);
    let fresh = lowered
        .quotas()
        .bulk_create(&ctx(), creates("pack-2", vec![draft("u3"), draft("u4")]))
        .await
        .expect_err("a new envelope meets the lowered limit");
    assert_eq!(fresh, DomainError::BulkTooLarge { items: 2, max: 1 });
}

#[tokio::test]
async fn the_size_check_comes_before_any_item_check() {
    let h = Harness::over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        Arc::new(InMemoryStorage::new()),
        limits(1),
    )
    .await;
    let mut invalid = draft("u2");
    invalid.metric = "not a metric".to_owned();
    let err = h
        .quotas()
        .bulk_create(&ctx(), creates("pack", vec![draft("u1"), invalid]))
        .await
        .expect_err("too large");
    assert_eq!(err, DomainError::BulkTooLarge { items: 2, max: 1 });
}

#[tokio::test]
async fn the_first_failing_item_is_reported_and_nothing_is_created() {
    let h = Harness::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let mut unknown = draft("u2");
    unknown.metric = "gts.cf.qe.metric.type.v1~cf.qe.metric.unknown.v1".to_owned();
    let mut foreign = draft("u3");
    foreign.tenant_id = quota_enforcement_sdk::TenantId::new(Uuid::from_u128(0xf0));
    let err = h
        .quotas()
        .bulk_create(&ctx(), creates("pack", vec![draft("u1"), unknown, foreign]))
        .await
        .expect_err("item 1 fails");
    let (index, cause) = item_error(&err);
    assert_eq!(index, 1, "the first failing item, not the last");
    assert!(
        matches!(cause, DomainError::MetricNotRegistered { .. }),
        "{cause:?}"
    );
    assert!(h.storage.events().is_empty(), "nothing of the envelope");

    let err = h
        .quotas()
        .bulk_create(
            &ctx(),
            creates(
                "pack-2",
                vec![draft("u1"), draft("u2"), {
                    let mut other = draft("u4");
                    other.tenant_id = quota_enforcement_sdk::TenantId::new(Uuid::from_u128(0xf0));
                    other
                }],
            ),
        )
        .await
        .expect_err("item 2 names another tenant");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "tenant_id",
            reason: tokens::BATCH_TENANT_MIXED,
        }
        .at_item(2)
    );
}

#[tokio::test]
async fn repeated_targets_and_item_keys_are_refused_at_the_second_occurrence() {
    let h = Harness::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let first = h.create_one("u1").await;
    let second = h.create_one("u2").await;
    let err = h
        .quotas()
        .bulk_update(
            &ctx(),
            updates(
                "raise",
                vec![
                    (first, cap_patch(200)),
                    (second, cap_patch(200)),
                    (first, cap_patch(300)),
                ],
            ),
        )
        .await
        .expect_err("a Quota named twice");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "quota_id",
            reason: tokens::BULK_QUOTA_DUPLICATE,
        }
        .at_item(2)
    );

    let mut request = deactivations("off", &[first, second]);
    for item in &mut request.items {
        item.idempotency_key = Some("same".to_owned());
    }
    let err = h
        .quotas()
        .bulk_deactivate(&ctx(), request)
        .await
        .expect_err("an item key used twice");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "idempotency_key",
            reason: tokens::BATCH_ITEM_KEY_DUPLICATE,
        }
        .at_item(1)
    );
}

#[tokio::test]
async fn each_update_item_is_authorized_as_a_single_update_is() {
    let permitting = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::new(permitting).await;
    let first = h.create_one("u1").await;
    let denied = h.create_one("u2").await;
    let pdp = Arc::new(DenyOnePdp {
        denied: denied.as_uuid(),
        calls: AtomicUsize::new(0),
    });
    let restricted = Harness::over(pdp.clone(), Arc::clone(&h.storage), limits(50)).await;
    let err = restricted
        .quotas()
        .bulk_update(
            &ctx(),
            updates(
                "raise",
                vec![(first, cap_patch(200)), (denied, cap_patch(200))],
            ),
        )
        .await
        .expect_err("item 1 is denied");
    let (index, cause) = item_error(&err);
    assert_eq!(index, 1);
    assert!(matches!(cause, DomainError::PdpDenied { .. }), "{cause:?}");
    assert_eq!(pdp.calls.load(Ordering::SeqCst), 2, "one check per item");
    assert_eq!(
        h.storage.quota(first).expect("first").cap,
        Some(100),
        "nothing was applied"
    );
}

#[tokio::test]
async fn a_replay_is_answered_only_after_the_pdp_permits_it() {
    let h = Harness::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let id = h.create_one("u1").await;
    let request = deactivations("off", &[id]);
    h.quotas()
        .bulk_deactivate(&ctx(), request.clone())
        .await
        .expect("bulk deactivate");
    let denying = Harness::over(Arc::new(DenyAllPdp), Arc::clone(&h.storage), limits(50)).await;
    let err = denying
        .quotas()
        .bulk_deactivate(&ctx(), request)
        .await
        .expect_err("no replay without a permit");
    let (index, cause) = item_error(&err);
    assert_eq!(index, 0);
    assert!(matches!(cause, DomainError::PdpDenied { .. }), "{cause:?}");
}

#[tokio::test]
async fn a_bulk_deactivate_of_an_unknown_quota_names_it() {
    let h = Harness::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let known = h.create_one("u1").await;
    let unknown = QuotaId::generate();
    let err = h
        .quotas()
        .bulk_deactivate(&ctx(), deactivations("off", &[known, unknown]))
        .await
        .expect_err("unknown item");
    assert_eq!(
        err,
        DomainError::NotFound {
            kind: ResourceKind::Quota,
            id: unknown.to_string(),
        }
        .at_item(1)
    );
    assert_eq!(
        h.storage.quota(known).expect("known").status,
        QuotaStatus::Active
    );
}

#[tokio::test]
async fn item_checks_report_the_first_failing_item_in_submission_order() {
    let h = Harness::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let ids = [
        h.create_one("u1").await,
        h.create_one("u2").await,
        h.create_one("u3").await,
    ];
    // A blank key at item 1 comes before the Quota repeated at item 3.
    let mut request = updates(
        "raise",
        vec![
            (ids[0], cap_patch(200)),
            (ids[1], cap_patch(200)),
            (ids[2], cap_patch(200)),
            (ids[0], cap_patch(300)),
        ],
    );
    request.items[1].idempotency_key = Some("  ".to_owned());
    let err = h
        .quotas()
        .bulk_update(&ctx(), request)
        .await
        .expect_err("item 1 fails first");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "idempotency_key",
            reason: tokens::IDEMPOTENCY_KEY_REQUIRED,
        }
        .at_item(1)
    );

    // An invalid draft at item 0 comes before the key repeated at item 2.
    let mut invalid = draft("u4");
    invalid.cap = Some(-1);
    let mut request = creates("pack", vec![invalid, draft("u5"), draft("u6")]);
    request.items[1].idempotency_key = Some("same".to_owned());
    request.items[2].idempotency_key = Some("same".to_owned());
    let err = h
        .quotas()
        .bulk_create(&ctx(), request)
        .await
        .expect_err("item 0 fails first");
    let (index, _) = item_error(&err);
    assert_eq!(index, 0, "{err:?}");
}

#[tokio::test]
async fn an_item_the_transport_could_not_convert_fails_after_the_envelope_checks() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::new(pdp.clone()).await;
    let unconverted = |index: usize| BulkCreateItem {
        idempotency_key: None,
        request: Err(DomainError::InvalidArgument {
            field: "cap",
            reason: tokens::CAP_OUT_OF_RANGE,
        }
        .at_item(index)),
    };
    let oversized = BulkCreateRequest {
        tenant_id: tenant(),
        idempotency_key: "pack".to_owned(),
        items: (0..=BULK_MAX_ITEMS_CEILING).map(unconverted).collect(),
    };
    assert_eq!(
        h.quotas()
            .bulk_create(&ctx(), oversized)
            .await
            .expect_err("too large"),
        DomainError::BulkTooLarge {
            items: BULK_MAX_ITEMS_CEILING + 1,
            max: BULK_MAX_ITEMS_CEILING
        },
        "the ceiling comes before any item"
    );
    assert_eq!(pdp.calls(), 0);

    let mut request = creates("pack", vec![draft("u1"), draft("u2")]);
    request.items[1].request = Err(DomainError::InvalidArgument {
        field: "cap",
        reason: tokens::CAP_OUT_OF_RANGE,
    });
    let err = h
        .quotas()
        .bulk_create(&ctx(), request)
        .await
        .expect_err("item 1 did not convert");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "cap",
            reason: tokens::CAP_OUT_OF_RANGE,
        }
        .at_item(1)
    );
}
