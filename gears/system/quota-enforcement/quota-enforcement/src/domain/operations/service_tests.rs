#![allow(clippy::expect_used)]

use super::ReturnRequest;
use std::sync::Arc;

use quota_enforcement_sdk::{
    CreditRequest, DecisionResult, EnforcementMode, NO_APPLICABLE_QUOTA, PreviewRequest,
    QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType, RollbackRequest,
    SubjectRef,
};
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::AccessScope;

use super::super::harness_tests::{Harness, attribution, debit, evaluation_limits, type_id};
use super::Operations;
use crate::domain::catalog::MetricMode;
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::ports::metrics::{DenialReason, OperationKind};
use crate::domain::tokens;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, DenyAllPdp, METRIC_REQUESTS, METRIC_TOKENS, PermitTenantsPdp,
    TENANT_SUBJECT_TYPE, USER_SUBJECT_TYPE, actor, fail_registry_reads, metric_type_document,
    tenant,
};

// --- debit ------------------------------------------------------------------

#[tokio::test]
async fn a_debit_applies_the_plan_and_reports_it_allowed() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let decision = h.operations().debit(debit(10, "k1")).await.expect("debit");

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
        .debit(debit(50, "k1"))
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
            .debit(debit(amount, "k1"))
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
        .debit(debit(1, "   "))
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
async fn an_operation_refused_on_the_contention_timeout_is_counted() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.storage
        .fail_with(quota_enforcement_sdk::StorageError::LeaseContentionTimeout);

    let error = h
        .operations()
        .credit(CreditRequest {
            tenant_id: tenant(),
            quota_id: id,
            amount: 1,
            idempotency_key: "c1".to_owned(),
        })
        .await
        .expect_err("contended");

    assert!(matches!(error, DomainError::LeaseContentionTimeout));
    assert_eq!(h.metrics.contention(), vec![OperationKind::Credit]);
}

#[tokio::test]
async fn a_replay_returns_the_stored_decision_and_moves_nothing() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let first = h
        .operations()
        .debit(debit(10, "k1"))
        .await
        .expect("first debit");

    let replay = h.operations().debit(debit(10, "k1")).await.expect("replay");

    assert_eq!(replay, first, "the stored decision, verbatim");
    assert_eq!(h.consumed(id), 10, "the replay moved nothing");
    assert_eq!(h.metrics.replays(), vec![OperationKind::Debit]);
}

#[tokio::test]
async fn a_divergent_payload_under_the_same_key_is_a_conflict() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(debit(10, "k1"))
        .await
        .expect("first debit");

    let error = h
        .operations()
        .debit(debit(11, "k1"))
        .await
        .expect_err("the same key carried a different amount");

    assert!(matches!(error, DomainError::IdempotencyPayloadMismatch));
    assert_eq!(h.consumed(id), 10);
}

#[tokio::test]
async fn one_key_with_other_subjects_is_a_conflict_and_not_a_second_charge() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(debit(10, "k1"))
        .await
        .expect("first debit");
    let mut other = debit(10, "k1");
    other.attribution.subjects[0].id = "u-2".to_owned();

    // A replica with a cold cache asks storage, which answers from the charge.
    let replica = Harness::new_over(&h).await;
    let error = replica
        .operations()
        .debit(other)
        .await
        .expect_err("the key is taken for the whole tenant");

    assert!(matches!(error, DomainError::IdempotencyPayloadMismatch));
    assert_eq!(h.consumed(id), 10);
}

#[tokio::test]
async fn a_no_applicable_quota_denial_is_not_cached_and_re_evaluates() {
    let h = Harness::new().await;

    let denied = h
        .operations()
        .debit(debit(5, "k1"))
        .await
        .expect("a denial is a successful call");
    assert_eq!(denied.denied_reason(), Some(NO_APPLICABLE_QUOTA));

    // Provisioning a Quota must change the answer for the very same request.
    let id = h.quota(Some(100)).await;
    let allowed = h
        .operations()
        .debit(debit(5, "k1"))
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
        .debit(debit(1, "k1"))
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
        .debit(debit(1, "k1"))
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
    h.operations().debit(debit(10, "k1")).await.expect("debit");

    let request = CreditRequest {
        tenant_id: tenant(),
        quota_id: id,
        amount: 4,
        idempotency_key: "c1".to_owned(),
    };
    let decision = h
        .operations()
        .credit(request.clone())
        .await
        .expect("credit");

    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 6);

    h.operations().credit(request).await.expect("replay");
    assert_eq!(h.consumed(id), 6, "the replay moved nothing");
    assert_eq!(h.metrics.replays(), vec![OperationKind::Credit]);
}

#[tokio::test]
async fn a_credit_of_a_non_positive_amount_is_refused() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let error = h
        .operations()
        .credit(CreditRequest {
            tenant_id: tenant(),
            quota_id: id,
            amount: 0,
            idempotency_key: "c1".to_owned(),
        })
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

fn rollback(original: &str) -> RollbackRequest {
    RollbackRequest {
        attribution: attribution(),
        original_operation: quota_enforcement_sdk::RollbackableOperation::Debit,
        original_idempotency_key: original.to_owned(),
    }
}

#[tokio::test]
async fn a_rollback_returns_what_remains_and_a_second_one_changes_nothing() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations().debit(debit(30, "k1")).await.expect("debit");

    let first = h
        .operations()
        .rollback(rollback("k1"))
        .await
        .expect("rollback");
    assert_eq!(first.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 0);

    let second = h
        .operations()
        .rollback(rollback("k1"))
        .await
        .expect("a second rollback succeeds");
    assert!(second.debit_plan.is_empty(), "nothing was left to return");
    assert_eq!(h.consumed(id), 0);
    assert_eq!(
        h.metrics.evaluations(),
        vec![
            OperationKind::Debit,
            OperationKind::Rollback,
            OperationKind::Rollback
        ]
    );
}

#[tokio::test]
async fn a_rollback_of_an_unknown_operation_is_not_found() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .rollback(rollback("never-happened"))
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

#[tokio::test]
async fn a_rollback_with_another_attribution_is_an_unknown_charge() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations().debit(debit(30, "k1")).await.expect("debit");
    let mut other = rollback("k1");
    other.attribution.metric.id = Some("another-resource".to_owned());

    let error = h
        .operations()
        .rollback(other)
        .await
        .expect_err("the charge was made under another attribution");
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
    assert_eq!(h.consumed(id), 30);
}

#[tokio::test]
async fn a_rollback_the_pdp_does_not_allow_is_refused_before_storage() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations().debit(debit(30, "k1")).await.expect("debit");
    let mut denied = Harness::with_pdp(Arc::new(DenyAllPdp)).await;
    denied.storage = Arc::clone(&h.storage);
    let scopes = h.storage.scopes().len();

    let error = denied
        .operations()
        .rollback(rollback("k1"))
        .await
        .expect_err("the caller may not roll back this attribution");
    assert!(matches!(error, DomainError::PdpDenied { .. }), "{error:?}");
    assert_eq!(h.storage.scopes().len(), scopes, "storage was not reached");
    assert_eq!(h.consumed(id), 30);
}

// --- return -----------------------------------------------------------------

fn give_back(charge: &str, amount: i64, key: &str) -> ReturnRequest {
    ReturnRequest {
        tenant_id: tenant(),
        operation: quota_enforcement_sdk::RollbackableOperation::Debit,
        charge_key: charge.to_owned(),
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
        amount,
        idempotency_key: key.to_owned(),
    }
}

#[tokio::test]
async fn a_return_gives_back_part_of_a_charge_once_per_key() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.trusted(None).debit(debit(30, "k1")).await.expect("debit");

    h.trusted(None)
        .return_amount(give_back("k1", 10, "r1"))
        .await
        .expect("return");
    h.trusted(None)
        .return_amount(give_back("k1", 10, "r1"))
        .await
        .expect("a repeat gets the first result");
    assert_eq!(h.consumed(id), 20);
    assert!(h.metrics.replays().contains(&OperationKind::Return));

    let error = h
        .trusted(None)
        .return_amount(give_back("k1", 11, "r1"))
        .await
        .expect_err("the key names another amount");
    assert!(matches!(error, DomainError::IdempotencyPayloadMismatch));
}

#[tokio::test]
async fn a_return_names_a_positive_amount_of_the_charge_metric_within_what_remains() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.trusted(None).debit(debit(30, "k1")).await.expect("debit");

    let zero = h
        .trusted(None)
        .return_amount(give_back("k1", 0, "r1"))
        .await
        .expect_err("zero");
    assert!(
        matches!(zero, DomainError::InvalidArgument { reason, .. } if reason == tokens::INVALID_AMOUNT)
    );
    let mut other = give_back("k1", 1, "r1");
    other.metric = quota_enforcement_sdk::MetricId::parse(METRIC_REQUESTS).expect("metric");
    let mismatch = h
        .trusted(None)
        .return_amount(other)
        .await
        .expect_err("another metric type");
    assert!(matches!(mismatch, DomainError::MetricTypeMismatch { .. }));
    let exceeds = h
        .trusted(None)
        .return_amount(give_back("k1", 31, "r1"))
        .await
        .expect_err("above the charge");
    assert!(matches!(
        exceeds,
        DomainError::ReturnExceedsCharge {
            requested: 31,
            remaining: 30
        }
    ));
    let unknown = h
        .trusted(None)
        .return_amount(give_back("nope", 1, "r1"))
        .await
        .expect_err("no charge");
    assert!(matches!(
        unknown,
        DomainError::NotFound {
            kind: ResourceKind::Operation,
            ..
        }
    ));
    assert_eq!(h.consumed(id), 30, "nothing changed");
}

// --- preview ----------------------------------------------------------------

#[tokio::test]
async fn a_preview_decides_without_persisting_anything() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let preview = h
        .operations()
        .preview(PreviewRequest {
            attribution: attribution(),
            amount: 10,
        })
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
        .preview(PreviewRequest {
            attribution: attribution(),
            amount: -5,
        })
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
        .debit(debit(0, "k1"))
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
    async fn tenant_quota(&self, cap: Option<u64>) -> QuotaId {
        self.storage
            .create_quota(
                &actor(),
                &AccessScope::allow_all(),
                QuotaDraft {
                    subject: SubjectRef {
                        subject_type: type_id(TENANT_SUBJECT_TYPE),
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
                subject_type: type_id(USER_SUBJECT_TYPE),
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

    h.operations().debit(debit(10, "k1")).await.expect("debit");

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
        .preview(PreviewRequest {
            attribution: attribution(),
            amount: 10,
        })
        .await
        .expect("preview");

    let planned: Vec<QuotaId> = preview
        .decision
        .debit_plan
        .iter()
        .flatten()
        .map(|entry| entry.quota_id)
        .collect();
    assert_eq!(
        planned,
        vec![user],
        "a preview that disagreed with its debit would be worse than none"
    );
}

#[tokio::test]
async fn a_failed_preview_names_the_engine_that_failed() {
    let h = Harness::new().await;
    h.quota(Some(1_000)).await;
    h.tenant_quota(Some(50)).await;
    let operations = Operations {
        evaluation: quota_enforcement_sdk::engine::EvaluationLimits {
            cost_limit: std::num::NonZeroU64::new(1).expect("nonzero"),
            ..evaluation_limits()
        },
        ..h.operations()
    };

    let error = operations
        .preview(PreviewRequest {
            attribution: attribution(),
            amount: 10,
        })
        .await
        .expect_err("two quotas cost more than the budget allows");

    assert!(
        matches!(
            &error,
            DomainError::EngineCostExceeded { engine_id } if engine_id == "most-restrictive-wins"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_replayed_denial_is_counted_once_however_the_replay_arrives() {
    let h = Harness::new().await;
    h.quota(Some(5)).await;

    let denied = h
        .operations()
        .debit(debit(50, "k1"))
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
        .debit(debit(50, "k1"))
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

#[tokio::test]
async fn a_replica_with_a_cold_cache_replays_or_refuses_from_the_stored_record() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let original = h.operations().debit(debit(10, "k1")).await.expect("debit");

    // A second replica: its replay cache is empty, so only the record answers.
    let replica = Harness::new_over(&h).await;
    let replayed = replica
        .operations()
        .debit(debit(10, "k1"))
        .await
        .expect("replay");
    assert_eq!(replayed, original, "the stored decision, verbatim");
    assert_eq!(h.consumed(id), 10, "applied once");
    assert_eq!(replica.metrics.replays(), vec![OperationKind::Debit]);

    let cold = Harness::new_over(&h).await;
    let err = cold
        .operations()
        .debit(debit(20, "k1"))
        .await
        .expect_err("the same key under another amount");
    assert!(
        matches!(err, DomainError::IdempotencyPayloadMismatch),
        "{err:?}"
    );
    assert_eq!(h.consumed(id), 10);
}

// --- metric types (ADR-0012, ADR-0013) --------------------------------------

fn make_unavailable(h: &Harness) {
    h.registry
        .seed(METRIC_TOKENS, abstract_metric(METRIC_TOKENS));
}

fn abstract_metric(id: &str) -> serde_json::Value {
    let mut document = metric_type_document(id, json!({ "kind": "counter" }), json!({}));
    document["x-gts-abstract"] = json!(true);
    document
}

fn failed_precondition(error: DomainError) -> bool {
    matches!(
        CanonicalError::from(error),
        CanonicalError::FailedPrecondition { .. }
    )
}

#[tokio::test]
async fn a_direct_metric_refuses_a_preview() {
    let h = Harness::over(
        Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])),
        MetricMode::Direct,
    )
    .await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .preview(PreviewRequest {
            attribution: attribution(),
            amount: 1,
        })
        .await
        .expect_err("direct");

    assert!(
        matches!(error, DomainError::MetricNotQuotaGated { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_request_for_an_unavailable_metric_is_a_precondition_and_changes_nothing() {
    let h = Harness::new().await;
    make_unavailable(&h);
    let id = h.quota(Some(100)).await;

    let error = h
        .operations()
        .debit(debit(1, "k1"))
        .await
        .expect_err("unavailable");

    assert!(
        matches!(error, DomainError::ProjectionNotResolvable { .. }),
        "{error:?}"
    );
    assert!(failed_precondition(error));
    assert_eq!(h.consumed(id), 0);
}

#[tokio::test]
async fn credit_and_rollback_run_while_the_metric_is_unavailable() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations().debit(debit(10, "k1")).await.expect("debit");
    let replica = Harness::new_over(&h).await;
    make_unavailable(&replica);
    assert!(
        replica.operations().debit(debit(1, "k2")).await.is_err(),
        "the replica holds the metric as unavailable"
    );

    replica
        .operations()
        .credit(CreditRequest {
            tenant_id: tenant(),
            quota_id: id,
            amount: 4,
            idempotency_key: "c1".to_owned(),
        })
        .await
        .expect("credit needs no catalogue entry");
    assert_eq!(h.consumed(id), 6);
    replica
        .operations()
        .rollback(rollback("k1"))
        .await
        .expect("rollback needs no catalogue entry");
    assert_eq!(h.consumed(id), 0);
}

#[tokio::test]
async fn a_silent_registry_spares_a_held_metric_and_refuses_one_not_held() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(debit(1, "k1"))
        .await
        .expect("first use reads the metric type");
    fail_registry_reads(&h.registry);

    h.operations()
        .debit(debit(1, "k2"))
        .await
        .expect("a held metric needs no registry read");
    assert_eq!(h.consumed(id), 2);

    let mut other = debit(1, "k3");
    METRIC_REQUESTS.clone_into(&mut other.attribution.metric.metric_type);
    other.attribution.metric.metadata = Some(serde_json::Map::new());
    let error = h
        .operations()
        .debit(other)
        .await
        .expect_err("a metric not held");
    assert!(
        matches!(error, DomainError::TypesRegistryUnavailable(_)),
        "{error:?}"
    );
    assert!(matches!(
        CanonicalError::from(error),
        CanonicalError::ServiceUnavailable { .. }
    ));
}

#[tokio::test]
async fn a_device_quota_is_in_the_subject_tier_ahead_of_the_tenant() {
    let h = Harness::new().await;
    let on_requests = |subject_type: &str, id: String, cap: u64| QuotaDraft {
        subject: SubjectRef {
            subject_type: type_id(subject_type),
            subject_id: id,
        },
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_REQUESTS).expect("metric"),
        ..Harness::draft(Some(cap))
    };
    let mut created = Vec::new();
    for draft in [
        on_requests(USER_SUBJECT_TYPE, "u-1".to_owned(), 1_000),
        on_requests(DEVICE_SUBJECT_TYPE, "d-1".to_owned(), 500),
        on_requests(TENANT_SUBJECT_TYPE, tenant().to_string(), 50),
    ] {
        created.push(
            h.storage
                .create_quota(&actor(), &AccessScope::allow_all(), draft, &[])
                .await
                .expect("quota"),
        );
    }
    let mut request = debit(10, "k1");
    METRIC_REQUESTS.clone_into(&mut request.attribution.metric.metric_type);
    request.attribution.metric.metadata = Some(serde_json::Map::new());
    request
        .attribution
        .subjects
        .push(quota_enforcement_sdk::SubjectClaim {
            subject_type: DEVICE_SUBJECT_TYPE.to_owned(),
            id: "d-1".to_owned(),
        });

    h.operations().debit(request).await.expect("debit");

    assert_eq!(
        h.consumed(created[1]),
        10,
        "the tightest subject-tier quota"
    );
    assert_eq!(h.consumed(created[0]), 0);
    assert_eq!(h.consumed(created[2]), 0, "the tenant tier was not reached");
}

#[tokio::test]
async fn a_quota_applies_only_to_a_request_its_conditions_hold_on() {
    let h = Harness::new().await;
    let id = h
        .quota_with(
            Some(100),
            json!({ "region": ["eu-west-1", "eu-central-1"] })
                .as_object()
                .cloned()
                .expect("object"),
        )
        .await;

    let allowed = h.operations().debit(debit(1, "k1")).await.expect("debit");
    assert_eq!(allowed.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 1);

    for (key, metadata) in [("k2", json!({ "region": "us-east-1" })), ("k3", json!({}))] {
        let mut request = debit(1, key);
        request.attribution.metric.metadata = metadata.as_object().cloned();
        let denied = h.operations().debit(request).await.expect("debit");
        assert_eq!(denied.denied_reason(), Some(NO_APPLICABLE_QUOTA), "{key}");
    }
    let mut attribution = attribution();
    attribution.metric.metadata = json!({ "region": "us-east-1" }).as_object().cloned();
    let preview = h
        .operations()
        .preview(PreviewRequest {
            attribution,
            amount: 1,
        })
        .await
        .expect("preview");
    assert_eq!(preview.decision.denied_reason(), Some(NO_APPLICABLE_QUOTA));
    assert_eq!(h.consumed(id), 1, "no counter change");
}

fn cel_policy_on_tokens(expr: &str) -> quota_enforcement_sdk::PolicyDraft {
    quota_enforcement_sdk::PolicyDraft {
        schema_snapshot: quota_enforcement_sdk::PolicySchemaSnapshot::default(),
        scope: quota_enforcement_sdk::PolicyScope::Metric {
            metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
        },
        engine_id: "cel".to_owned(),
        engine_config: json!({ "expr": expr }),
        timeout_ms: Some(60_000),
        description: None,
        comment: None,
        created_by: "operator".to_owned(),
    }
}

fn revise_tokens(h: &Harness, traits: serde_json::Value, properties: serde_json::Value) {
    h.registry.seed_entity(
        types_registry_sdk::testing_platform::Seed::new(
            METRIC_TOKENS,
            metric_type_document(METRIC_TOKENS, traits, properties),
        )
        .version(2),
    );
}

#[tokio::test]
async fn a_revision_an_active_policy_does_not_fit_rejects_the_request_and_counts_it() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.storage
        .create_policy(
            &actor(),
            cel_policy_on_tokens(
                "request.region == 'eu-west-1' \
                    ? {'debit_plan': [[{'id': quotas[0].id, 'amount': amount}]]} \
                    : {'deny': {'reason': 'REGION'}}",
            ),
            &[],
        )
        .await
        .expect("policy");
    h.operations()
        .debit(debit(1, "k1"))
        .await
        .expect("the policy fits the type it was written against");
    revise_tokens(
        &h,
        json!({ "kind": "counter", "admitted_subjects": [USER_SUBJECT_TYPE] }),
        json!({ "zone": { "type": "string" } }),
    );
    h.catalog.refresh().await.expect("scheduled read");

    let mut request = debit(1, "k2");
    request.attribution.metric.metadata = json!({ "zone": "a" }).as_object().cloned();
    let error = h
        .operations()
        .debit(request)
        .await
        .expect_err("the policy cannot read the revised metric object");

    assert!(
        matches!(error, DomainError::PolicyEvaluationFailed { .. }),
        "{error:?}"
    );
    assert!(failed_precondition(error));
    assert_eq!(h.consumed(id), 1, "no counter change");
    assert_eq!(h.metrics.policy_evaluation_failures(), [METRIC_TOKENS]);
    assert!(
        matches!(
            h.catalog
                .peek(&quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric")),
            Some(crate::domain::catalog::CatalogEntry::Served(_))
        ),
        "the metric stays served"
    );
}

#[tokio::test]
async fn a_policy_decides_by_the_id_of_the_requests_metric() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.storage
        .create_policy(
            &actor(),
            cel_policy_on_tokens(
                "metric_id == 'blocked' \
                    ? {'deny': {'reason': 'BLOCKED'}} \
                    : {'debit_plan': [[{'id': quotas[0].id, 'amount': amount}]]}",
            ),
            &[],
        )
        .await
        .expect("policy");

    let mut blocked = debit(1, "k1");
    blocked.attribution.metric.id = Some("blocked".to_owned());
    let denied = h.operations().debit(blocked).await.expect("debit");
    assert_eq!(denied.denied_reason(), Some("BLOCKED"));

    let mut other = debit(1, "k2");
    other.attribution.metric.id = Some("other".to_owned());
    h.operations().debit(other).await.expect("debit");
    assert_eq!(h.consumed(id), 1, "only the second debit is charged");
}

#[tokio::test]
async fn a_revision_that_stops_admitting_the_subject_type_of_a_quota_rejects_its_requests() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(debit(1, "k1"))
        .await
        .expect("admitted");
    revise_tokens(
        &h,
        json!({ "kind": "counter" }),
        json!({ "region": { "type": "string" }, "model": { "type": "string" } }),
    );
    h.catalog.refresh().await.expect("scheduled read");

    let error = h
        .operations()
        .debit(debit(1, "k2"))
        .await
        .expect_err("the user type is no longer admitted");

    assert!(
        matches!(
            error,
            DomainError::InvalidArgument { reason, .. } if reason == tokens::METRIC_NOT_ADMITTED
        ),
        "{error:?}"
    );
    assert_eq!(h.consumed(id), 1);
    assert_eq!(
        h.storage.quota(id).map(|quota| quota.status),
        Some(quota_enforcement_sdk::QuotaStatus::Active),
        "the quota stays stored and active"
    );
}

#[tokio::test]
async fn a_retry_that_lists_the_subjects_differently_replays_the_first_result() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let first = h.operations().debit(debit(10, "k1")).await.expect("debit");
    let tenant_claim = quota_enforcement_sdk::SubjectClaim {
        subject_type: TENANT_SUBJECT_TYPE.to_owned(),
        id: tenant().to_string(),
    };

    // The tenant named explicitly, before and after the user: the same set.
    for tenant_first in [true, false] {
        let mut retry = debit(10, "k1");
        if tenant_first {
            retry.attribution.subjects.insert(0, tenant_claim.clone());
        } else {
            retry.attribution.subjects.push(tenant_claim.clone());
        }
        let replayed = h.operations().debit(retry).await.expect("replay");
        assert_eq!(replayed, first, "tenant first: {tenant_first}");
    }
    assert_eq!(h.consumed(id), 10, "charged once");

    // Another set under the same key is still a conflict.
    let mut other = debit(10, "k1");
    other.attribution.subjects[0].id = "u-2".to_owned();
    let error = h
        .operations()
        .debit(other)
        .await
        .expect_err("another subject set");
    assert!(
        matches!(error, DomainError::IdempotencyPayloadMismatch),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_integer_written_as_a_float_is_the_same_integer_for_conditions_and_replay() {
    let h = Harness::new().await;
    revise_tokens(
        &h,
        json!({ "kind": "counter", "admitted_subjects": [USER_SUBJECT_TYPE] }),
        json!({ "tier": { "type": "integer" } }),
    );
    let conditions = json!({ "tier": 5 }).as_object().cloned().expect("object");
    let id = h.quota_with(Some(100), conditions).await;
    let tiered = |tier: serde_json::Value, key: &str| {
        let mut request = debit(10, key);
        request.attribution.metric.metadata = json!({ "tier": tier }).as_object().cloned();
        request
    };

    let first = h
        .operations()
        .debit(tiered(json!(5.0), "k1"))
        .await
        .expect("debit");
    assert!(
        matches!(first.result, DecisionResult::Allowed),
        "the Quota conditioned on tier 5 applies: {first:?}"
    );
    assert_eq!(h.consumed(id), 10);

    let replayed = h
        .operations()
        .debit(tiered(json!(5), "k1"))
        .await
        .expect("the same request, written another way");
    assert_eq!(replayed, first);
    assert_eq!(h.consumed(id), 10, "charged once");
}

#[tokio::test]
async fn an_idempotency_key_longer_than_the_bound_is_refused_before_storage() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    h.operations()
        .debit(debit(1, &"k".repeat(256)))
        .await
        .expect("a key of the bound");
    let error = h
        .operations()
        .debit(debit(1, &"k".repeat(257)))
        .await
        .expect_err("a key past the bound");

    assert_eq!(
        error,
        DomainError::InvalidArgument {
            field: "idempotency_key",
            reason: tokens::IDEMPOTENCY_KEY_TOO_LONG,
        }
    );
    assert_eq!(h.consumed(id), 1, "nothing was written for the long key");
}
