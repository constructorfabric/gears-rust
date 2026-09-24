#![allow(clippy::expect_used)]

use std::sync::Arc;

use quota_enforcement_sdk::{
    CreditRequest, DecisionResult, EnforcementMode, NO_APPLICABLE_QUOTA, PreviewRequest,
    QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType, RollbackRequest,
    SubjectRef,
};
use toolkit_security::AccessScope;

use super::super::harness_tests::{Harness, attribution, debit, type_id};
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::ports::metric_registry::MetricMode;
use crate::domain::ports::metrics::{DenialReason, OperationKind};
use crate::domain::tokens;
use crate::test_support::{
    DenyAllPdp, LLM_TENANT_PROJECTION, LLM_USER_PROJECTION, METRIC_TOKENS, PermitTenantsPdp, ctx,
    tenant,
};

// --- debit ------------------------------------------------------------------

#[tokio::test]
async fn a_debit_applies_the_plan_and_reports_it_allowed() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let decision = h
        .operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("debit");

    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 10);
    assert_eq!(h.metrics.evaluations(), vec![OperationKind::Debit]);
}

#[tokio::test]
async fn a_denial_is_a_successful_call_that_counts_its_reason() {
    let h = Harness::new().await;
    let id = h.quota(Some(5)).await;

    let decision = h
        .operations()
        .debit(&ctx(), debit(50, "k1"))
        .await
        .expect("a denial is not an error");

    assert!(matches!(decision.result, DecisionResult::Denied { .. }));
    assert_eq!(h.consumed(id), 0, "a denial moves no counter");
    assert!(
        h.metrics.denials().contains(&DenialReason::QuotaExceeded),
        "the closed reason is counted, got {:?}",
        h.metrics.denials()
    );
}

#[tokio::test]
async fn a_non_positive_amount_is_refused_before_the_pdp_and_storage() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp)).await;

    for amount in [0, -1] {
        let error = h
            .operations()
            .debit(&ctx(), debit(amount, "k1"))
            .await
            .expect_err("a non-positive amount cannot be charged");

        assert!(
            matches!(
                error,
                DomainError::InvalidArgument {
                    field: "amount",
                    reason
                } if reason == tokens::INVALID_AMOUNT
            ),
            "got {error:?}"
        );
    }
    // Refused before the PDP: a denying PDP never got the chance to answer.
    assert!(
        h.metrics
            .denials()
            .iter()
            .all(|reason| *reason == DenialReason::InvalidArgument)
    );
}

#[tokio::test]
async fn a_missing_idempotency_key_is_refused() {
    let h = Harness::new().await;

    let error = h
        .operations()
        .debit(&ctx(), debit(1, "   "))
        .await
        .expect_err("every write carries a key");

    assert!(matches!(
        error,
        DomainError::InvalidArgument {
            field: "idempotency_key",
            ..
        }
    ));
}

#[tokio::test]
async fn a_replay_returns_the_stored_decision_and_moves_nothing() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let first = h
        .operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("first debit");

    let replay = h
        .operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("replay");

    assert_eq!(replay, first, "the stored decision, verbatim");
    assert_eq!(h.consumed(id), 10, "the replay moved nothing");
    assert_eq!(h.metrics.replays(), vec![OperationKind::Debit]);
}

#[tokio::test]
async fn a_divergent_payload_under_the_same_key_is_a_conflict() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("first debit");

    let error = h
        .operations()
        .debit(&ctx(), debit(11, "k1"))
        .await
        .expect_err("the same key carried a different amount");

    assert!(matches!(error, DomainError::IdempotencyPayloadMismatch));
    assert_eq!(h.consumed(id), 10);
}

#[tokio::test]
async fn a_no_applicable_quota_denial_is_not_cached_and_re_evaluates() {
    let h = Harness::new().await;

    let denied = h
        .operations()
        .debit(&ctx(), debit(5, "k1"))
        .await
        .expect("a denial is a successful call");
    assert_eq!(denied.denied_reason(), Some(NO_APPLICABLE_QUOTA));

    // Provisioning a Quota must change the answer for the very same request.
    let id = h.quota(Some(100)).await;
    let allowed = h
        .operations()
        .debit(&ctx(), debit(5, "k1"))
        .await
        .expect("re-evaluated");

    assert_eq!(allowed.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 5);
}

#[tokio::test]
async fn a_metric_that_is_not_quota_gated_is_refused_before_storage() {
    let h = Harness::over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        MetricMode::Direct,
    )
    .await;
    let id = h.quota(Some(100)).await;

    let error = h
        .operations()
        .debit(&ctx(), debit(1, "k1"))
        .await
        .expect_err("a direct metric is recorded elsewhere");

    assert!(matches!(error, DomainError::MetricNotQuotaGated { .. }));
    assert_eq!(h.consumed(id), 0);
}

#[tokio::test]
async fn a_pdp_denial_fails_the_debit() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp)).await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .debit(&ctx(), debit(1, "k1"))
        .await
        .expect_err("the PDP refused the attribution");

    assert!(
        matches!(error, DomainError::PdpDenied { .. }),
        "got {error:?}"
    );
}

// --- credit -----------------------------------------------------------------

#[tokio::test]
async fn a_credit_lowers_the_counter_and_replays_as_a_no_op() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("debit");

    let request = CreditRequest {
        tenant_id: tenant(),
        quota_id: id,
        amount: 4,
        idempotency_key: "c1".to_owned(),
    };
    let decision = h
        .operations()
        .credit(&ctx(), request.clone())
        .await
        .expect("credit");

    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 6);

    h.operations()
        .credit(&ctx(), request)
        .await
        .expect("replay");
    assert_eq!(h.consumed(id), 6, "the replay moved nothing");
    assert_eq!(h.metrics.replays(), vec![OperationKind::Credit]);
}

#[tokio::test]
async fn a_credit_of_a_non_positive_amount_is_refused() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let error = h
        .operations()
        .credit(
            &ctx(),
            CreditRequest {
                tenant_id: tenant(),
                quota_id: id,
                amount: 0,
                idempotency_key: "c1".to_owned(),
            },
        )
        .await
        .expect_err("zero is not an amount");

    assert!(matches!(
        error,
        DomainError::InvalidArgument {
            field: "amount",
            ..
        }
    ));
}

// --- rollback ---------------------------------------------------------------

fn rollback(original: &str, key: &str) -> RollbackRequest {
    RollbackRequest {
        attribution: attribution(),
        // The default namespace: these reverse direct debits.
        original_operation: quota_enforcement_sdk::RollbackableOperation::Debit,
        original_idempotency_key: original.to_owned(),
        idempotency_key: key.to_owned(),
    }
}

#[tokio::test]
async fn a_rollback_reverses_its_debit_and_replays_as_a_no_op() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(&ctx(), debit(30, "k1"))
        .await
        .expect("debit");

    h.operations()
        .rollback(&ctx(), rollback("k1", "r1"))
        .await
        .expect("rollback");
    assert_eq!(h.consumed(id), 0);

    h.operations()
        .rollback(&ctx(), rollback("k1", "r1"))
        .await
        .expect("replay");
    assert_eq!(h.consumed(id), 0);
    assert!(h.metrics.replays().contains(&OperationKind::Rollback));
}

#[tokio::test]
async fn a_rollback_of_an_unknown_operation_is_not_found() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .rollback(&ctx(), rollback("never-happened", "r1"))
        .await
        .expect_err("no committed debit answers that key");

    assert!(
        matches!(
            error,
            DomainError::NotFound {
                kind: ResourceKind::Operation,
                ..
            }
        ),
        "got {error:?}"
    );
}

// --- preview ----------------------------------------------------------------

#[tokio::test]
async fn a_preview_decides_without_persisting_anything() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let preview = h
        .operations()
        .preview(
            &ctx(),
            PreviewRequest {
                attribution: attribution(),
                amount: 10,
            },
        )
        .await
        .expect("preview");

    assert!(preview.preview, "a dry run says so");
    assert_eq!(preview.decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 0, "a preview mutates nothing");
    assert_eq!(h.metrics.evaluations(), vec![OperationKind::Preview]);
}

#[tokio::test]
async fn a_preview_of_a_non_positive_amount_is_refused_like_a_debit() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .preview(
            &ctx(),
            PreviewRequest {
                attribution: attribution(),
                amount: -5,
            },
        )
        .await
        .expect_err("a negative amount cannot be previewed either");

    assert!(matches!(
        error,
        DomainError::InvalidArgument {
            field: "amount",
            ..
        }
    ));
}

#[tokio::test]
async fn latency_is_recorded_for_a_failure_as_well_as_a_success() {
    let h = Harness::new().await;

    h.operations()
        .debit(&ctx(), debit(0, "k1"))
        .await
        .expect_err("refused");

    assert_eq!(
        h.metrics.evaluations(),
        vec![OperationKind::Debit],
        "a slow refusal is as interesting as a slow commit"
    );
}

// --- subject tiers ----------------------------------------------------------

impl Harness {
    /// A tenant-scoped Quota on the same metric, so both tiers apply to one
    /// request and the engine has to choose between them.
    async fn tenant_quota(&self, cap: Option<u64>) -> QuotaId {
        self.storage
            .create_quota(
                &ctx(),
                &AccessScope::allow_all(),
                QuotaDraft {
                    subject: SubjectRef {
                        projection_type: type_id(LLM_TENANT_PROJECTION),
                        subject_id: tenant().to_string(),
                    },
                    cap,
                    ..Self::draft(cap)
                },
                &[],
            )
            .await
            .expect("tenant quota")
    }

    fn draft(cap: Option<u64>) -> QuotaDraft {
        QuotaDraft {
            tenant_id: tenant(),
            subject: SubjectRef {
                projection_type: type_id(LLM_USER_PROJECTION),
                subject_id: "u-1".to_owned(),
            },
            metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
            quota_type: QuotaType::Allocation,
            period: None,
            enforcement_mode: EnforcementMode::Hard,
            cap,
            notification_thresholds: Vec::new(),
            validity_window: None,
            fail_open_hint: false,
            metadata: serde_json::Map::new(),
            source: QuotaSource::Operator,
            constraint_contract: quota_enforcement_sdk::ContractRef {
                type_id: type_id(crate::test_support::LLM_TOKEN_CONSTRAINT),
                version: 1,
            },
        }
    }
}

#[tokio::test]
async fn a_user_quota_binds_ahead_of_a_tighter_tenant_quota() {
    let h = Harness::new().await;
    // The tenant Quota is the tighter of the two, so the remaining-capacity
    // tie-break would pick it. Only the tier ordering makes the user Quota
    // bind, which is what this asserts.
    let user = h.quota(Some(1_000)).await;
    let tenant_wide = h.tenant_quota(Some(50)).await;

    h.operations()
        .debit(&ctx(), debit(10, "k1"))
        .await
        .expect("debit");

    assert_eq!(h.consumed(user), 10, "the user tier bound the request");
    assert_eq!(
        h.consumed(tenant_wide),
        0,
        "the tenant fallback was not charged despite being tighter"
    );
}

#[tokio::test]
async fn a_preview_resolves_the_same_tier_the_debit_would() {
    let h = Harness::new().await;
    let user = h.quota(Some(1_000)).await;
    h.tenant_quota(Some(50)).await;

    let preview = h
        .operations()
        .preview(
            &ctx(),
            PreviewRequest {
                attribution: attribution(),
                amount: 10,
            },
        )
        .await
        .expect("preview");

    let planned: Vec<QuotaId> = preview.decision.debit_plan.keys().copied().collect();
    assert_eq!(
        planned,
        vec![user],
        "a preview that disagreed with its debit would be worse than none"
    );
}

#[tokio::test]
async fn a_replayed_denial_is_counted_once_however_the_replay_arrives() {
    let h = Harness::new().await;
    h.quota(Some(5)).await;

    let denied = h
        .operations()
        .debit(&ctx(), debit(50, "k1"))
        .await
        .expect("a denial is a successful call");
    assert!(matches!(denied.result, DecisionResult::Denied { .. }));

    // The replay a caller cannot stage by hand: another writer commits the
    // record between this replica's lookup and its transaction, so the
    // pre-check misses and the decision comes back from inside the row locks.
    let raced = Harness::new_over(&h).await;
    raced.storage.hide_idempotency_lookups();
    let replayed = raced
        .operations()
        .debit(&ctx(), debit(50, "k1"))
        .await
        .expect("the transaction found the record its caller missed");

    assert_eq!(replayed, denied, "the stored decision, verbatim");
    assert_eq!(
        raced.metrics.replays(),
        vec![OperationKind::Debit],
        "it is reported as the replay it is"
    );
    assert_eq!(
        raced.metrics.denials(),
        Vec::new(),
        "a replay is one operation reported twice, not a second denial"
    );
}
