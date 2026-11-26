#![allow(clippy::expect_used)]
//! The atomic batch debit over real engines and the in-memory double:
//! fail-fast order, evaluate-all, the union applied or nothing, replay ahead
//! of the `mode` and size checks, the timeout, and the telemetry it leaves.

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{
    BatchDebitRequest, BatchDecision, BatchItemRequest, BatchMode, BatchResult, DecisionResult,
    EvaluationAttribution, TenantId,
};
use uuid::Uuid;

use super::super::harness_tests::{Harness, attribution, debit};
use crate::domain::catalog::MetricMode;
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;
use crate::test_support::{DenyAllPdp, PermitTenantsPdp, SpanCapture, TENANT_SUBJECT_TYPE, tenant};

fn item(amount: i64, key: &str) -> BatchItemRequest {
    BatchItemRequest {
        attribution: attribution(),
        amount,
        idempotency_key: key.to_owned(),
    }
}

fn batch(items: Vec<BatchItemRequest>, key: &str) -> BatchDebitRequest {
    BatchDebitRequest {
        mode: BatchMode::Atomic,
        items,
        idempotency_key: key.to_owned(),
    }
}

fn invalid(error: &DomainError, field: &str, reason: &str) -> bool {
    matches!(
        error,
        DomainError::InvalidArgument { field: f, reason: r } if *f == field && *r == reason
    )
}

fn invalid_item(error: &DomainError, index: usize, field: &str, reason: &str) -> bool {
    matches!(
        error,
        DomainError::InvalidBatchItem { index: i, field: f, reason: r }
            if *i == index && *f == field && *r == reason
    )
}

fn allowed(decision: &BatchDecision) -> Vec<bool> {
    decision
        .items
        .iter()
        .map(|item| item.decision.result == DecisionResult::Allowed)
        .collect()
}

// --- the envelope -------------------------------------------------------------

#[tokio::test]
async fn an_allowed_batch_applies_every_item_and_echoes_their_keys() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let decision = h
        .operations()
        .batch_debit(batch(vec![item(30, "i1"), item(40, "i2")], "b1"))
        .await
        .expect("batch");

    assert_eq!(decision.result, BatchResult::Allowed);
    assert_eq!(allowed(&decision), vec![true, true]);
    let keys: Vec<&str> = decision
        .items
        .iter()
        .map(|item| item.idempotency_key.as_str())
        .collect();
    assert_eq!(keys, vec!["i1", "i2"]);
    assert_eq!(h.consumed(id), 70, "the union of both plans");
    assert!(h.metrics.denials().is_empty());
    assert!(
        h.metrics.evaluations().is_empty(),
        "no operation-labelled series for a batch"
    );
}

#[tokio::test]
async fn a_batch_opens_the_same_spans_for_one_item_and_for_fifty() {
    let h = Harness::new().await;
    h.quota(Some(10_000)).await;
    // Warm the artifact cache, so neither traced batch prepares the policy.
    let warm = h
        .operations()
        .batch_debit(batch(vec![item(1, "w")], "warm"))
        .await
        .expect("warm");
    assert_eq!(warm.result, BatchResult::Allowed);
    let capture = SpanCapture::default();

    let single = capture
        .within(
            h.operations()
                .batch_debit(batch(vec![item(1, "a0")], "one")),
        )
        .await
        .expect("one item");
    assert_eq!(single.result, BatchResult::Allowed);
    let one = capture.names();
    capture.clear();
    let fifty = (0..50).map(|i| item(1, &format!("b{i}"))).collect();
    let many = capture
        .within(h.operations().batch_debit(batch(fifty, "fifty")))
        .await
        .expect("fifty items");
    assert_eq!(many.result, BatchResult::Allowed);

    assert_eq!(capture.names(), one, "no span per item");
    assert!(
        one.contains(&"quota_enforcement.operations.batch_debit".to_owned()),
        "{one:?}"
    );
    let admission = capture
        .spans()
        .into_iter()
        .find(|s| s.name == "quota_enforcement.admission")
        .expect("one admission span");
    assert_eq!(admission.item_count, Some(50), "it covers every item");
    assert_eq!(
        admission.parent.as_deref(),
        Some("quota_enforcement.operations.batch_debit")
    );
}

#[tokio::test]
async fn a_denied_item_denies_the_batch_moves_nothing_and_is_counted_once() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    // The first item fits; the second fits alone but not after the first.
    let decision = h
        .operations()
        .batch_debit(batch(
            vec![item(60, "i1"), item(60, "i2"), item(60, "i3")],
            "b1",
        ))
        .await
        .expect("batch");

    assert_eq!(decision.result, BatchResult::Denied);
    assert_eq!(
        allowed(&decision),
        vec![true, false, false],
        "every item is evaluated and reported, the ones after the denial included"
    );
    assert_eq!(h.consumed(id), 0, "nothing moves");
    assert_eq!(h.metrics.denials().len(), 1, "one denial per batch");
}

#[tokio::test]
async fn an_item_after_a_denied_one_sees_only_the_allowed_items_before_it() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;

    let decision = h
        .operations()
        .batch_debit(batch(vec![item(150, "i1"), item(90, "i2")], "b1"))
        .await
        .expect("batch");

    assert_eq!(decision.result, BatchResult::Denied);
    assert_eq!(
        allowed(&decision),
        vec![false, true],
        "the denied item's plan adds nothing to the running state"
    );
}

#[tokio::test]
async fn a_replay_returns_the_stored_batch_from_another_replica() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let request = batch(vec![item(60, "i1"), item(60, "i2")], "b1");
    let first = h
        .operations()
        .batch_debit(request.clone())
        .await
        .expect("batch");

    let replica = Harness::new_over(&h).await;
    let again = replica
        .operations()
        .batch_debit(request)
        .await
        .expect("replay");

    assert_eq!(again, first);
    assert_eq!(h.consumed(id), 0);
    assert!(
        replica.metrics.denials().is_empty(),
        "a replay is not a second denial"
    );
}

#[tokio::test]
async fn a_batch_retry_that_lists_the_subjects_differently_replays_the_first_result() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let first = h
        .operations()
        .batch_debit(batch(vec![item(10, "i1")], "b1"))
        .await
        .expect("batch");

    // The tenant named explicitly, before and after the user: the same set.
    let tenant_claim = quota_enforcement_sdk::SubjectClaim {
        subject_type: TENANT_SUBJECT_TYPE.to_owned(),
        id: tenant().to_string(),
    };
    for tenant_first in [true, false] {
        let mut retry = item(10, "i1");
        if tenant_first {
            retry.attribution.subjects.insert(0, tenant_claim.clone());
        } else {
            retry.attribution.subjects.push(tenant_claim.clone());
        }
        let replayed = h
            .operations()
            .batch_debit(batch(vec![retry], "b1"))
            .await
            .expect("replay");
        assert_eq!(replayed, first, "tenant first: {tenant_first}");
    }
    assert_eq!(h.consumed(id), 10, "charged once");
}

#[tokio::test]
async fn a_key_reused_for_a_different_batch_is_a_payload_mismatch() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let stored = h
        .operations()
        .batch_debit(batch(vec![item(10, "i1")], "b1"))
        .await
        .expect("batch");
    assert_eq!(stored.result, BatchResult::Allowed);

    let error = h
        .operations()
        .batch_debit(batch(vec![item(11, "i1")], "b1"))
        .await
        .expect_err("mismatch");

    assert_eq!(error, DomainError::IdempotencyPayloadMismatch);
}

// --- item keys ----------------------------------------------------------------

#[tokio::test]
async fn a_batch_whose_item_key_names_a_single_debit_is_refused_whole() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    h.operations()
        .debit(debit(10, "charged"))
        .await
        .expect("an allowed debit");
    let denied = h
        .operations()
        .debit(debit(1_000, "denied"))
        .await
        .expect("a denied debit");
    assert!(matches!(denied.result, DecisionResult::Denied { .. }));

    for key in ["charged", "denied"] {
        let error = h
            .operations()
            .batch_debit(batch(vec![item(1, "fresh"), item(10, key)], key))
            .await
            .expect_err("a batch never answers with a single debit's result");
        assert_eq!(
            error,
            DomainError::IdempotencyPayloadMismatch.at_item(1),
            "{key}"
        );
        let wire = toolkit_canonical_errors::CanonicalError::from(error);
        assert!(
            wire.to_string().contains("items[1]"),
            "the answer names the item: {wire}"
        );
    }
    assert_eq!(h.consumed(id), 10, "nothing moved");
}

#[tokio::test]
async fn a_duplicate_of_a_batch_committed_since_the_envelope_lookup_gets_its_first_result() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let request = batch(vec![item(30, "i1"), item(40, "i2")], "b1");
    let first = h
        .operations()
        .batch_debit(request.clone())
        .await
        .expect("batch");

    // The envelope lookup misses as if the batch committed just after it, so
    // the item keys find the batch's own charges.
    h.storage.hide_next_idempotency_lookups(1);
    let again = h
        .operations()
        .batch_debit(request)
        .await
        .expect("the batch's first result, not a mismatch");

    assert_eq!(again, first);
    assert_eq!(h.consumed(id), 70);
}

#[tokio::test]
async fn a_denied_batch_replayed_inside_the_transaction_is_not_a_second_denial() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let request = batch(vec![item(60, "i1"), item(60, "i2")], "b1");
    let first = h
        .operations()
        .batch_debit(request.clone())
        .await
        .expect("batch");
    assert_eq!(first.result, BatchResult::Denied);

    h.storage.hide_next_idempotency_lookups(1);
    let again = h.operations().batch_debit(request).await.expect("replay");

    assert_eq!(again, first);
    assert_eq!(h.metrics.denials().len(), 1, "one denial for one batch");
}

#[tokio::test]
async fn a_single_debit_under_an_item_key_gets_the_items_result() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let stored = h
        .operations()
        .batch_debit(batch(vec![item(30, "i1")], "b1"))
        .await
        .expect("batch");

    let replay = h
        .operations()
        .debit(debit(30, "i1"))
        .await
        .expect("the item's charge answers");
    assert_eq!(replay, stored.items[0].decision);
    assert_eq!(
        h.operations()
            .debit(debit(31, "i1"))
            .await
            .expect_err("another fingerprint"),
        DomainError::IdempotencyPayloadMismatch
    );
    assert_eq!(h.consumed(id), 30);
}

// --- fail-fast order ----------------------------------------------------------

#[tokio::test]
async fn an_empty_batch_is_refused_before_its_key_is_looked_at() {
    let h = Harness::new().await;

    let error = h
        .operations()
        .batch_debit(batch(Vec::new(), " "))
        .await
        .expect_err("empty");

    assert!(invalid(&error, "items", tokens::BATCH_EMPTY), "{error:?}");
    assert_eq!(h.metrics.denials(), vec![DenialReason::InvalidArgument]);
}

#[tokio::test]
async fn a_bad_amount_names_its_item_before_the_envelope_key_is_checked() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let error = h
        .operations()
        .batch_debit(batch(vec![item(5, "i1"), item(0, "i2")], ""))
        .await
        .expect_err("amount");

    assert!(
        invalid_item(&error, 1, "amount", tokens::INVALID_AMOUNT),
        "{error:?}"
    );
    assert_eq!(h.consumed(id), 0);
}

#[tokio::test]
async fn a_blank_envelope_key_is_refused_before_the_item_keys() {
    let h = Harness::new().await;

    let error = h
        .operations()
        .batch_debit(batch(vec![item(5, " ")], " "))
        .await
        .expect_err("key");

    assert!(
        invalid(&error, "idempotency_key", tokens::IDEMPOTENCY_KEY_REQUIRED),
        "{error:?}"
    );
}

#[tokio::test]
async fn blank_and_duplicate_item_keys_are_refused() {
    let h = Harness::new().await;
    let ops = h.operations();

    let blank = ops
        .batch_debit(batch(vec![item(5, "i1"), item(5, "")], "b1"))
        .await
        .expect_err("blank");
    let duplicate = ops
        .batch_debit(batch(vec![item(5, "i1"), item(5, "i1")], "b1"))
        .await
        .expect_err("duplicate");

    assert!(
        invalid_item(
            &blank,
            1,
            "idempotency_key",
            tokens::IDEMPOTENCY_KEY_REQUIRED
        ),
        "{blank:?}"
    );
    assert!(
        invalid(&duplicate, "items", tokens::BATCH_ITEM_KEY_DUPLICATE),
        "{duplicate:?}"
    );
}

#[tokio::test]
async fn a_batch_across_tenants_is_refused_before_any_pdp_call() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;
    let other = BatchItemRequest {
        attribution: EvaluationAttribution {
            tenant_id: TenantId::new(Uuid::from_u128(0xbad)),
            ..attribution()
        },
        ..item(5, "i2")
    };

    let error = h
        .operations()
        .batch_debit(batch(vec![item(5, "i1"), other], "b1"))
        .await
        .expect_err("mixed");

    assert!(
        invalid(&error, "items", tokens::BATCH_TENANT_MIXED),
        "{error:?}"
    );
    assert_eq!(pdp.calls(), 0);
}

#[tokio::test]
async fn one_item_the_pdp_refuses_fails_the_whole_envelope() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp)).await;

    let error = h
        .operations()
        .batch_debit(batch(vec![item(5, "i1")], "b1"))
        .await
        .expect_err("refused");

    assert!(matches!(error, DomainError::PdpDenied { .. }), "{error:?}");
}

#[tokio::test]
async fn an_item_on_a_directly_recorded_metric_fails_the_envelope() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::over(pdp, MetricMode::Direct).await;

    let error = h
        .operations()
        .batch_debit(batch(vec![item(5, "i1")], "b1"))
        .await
        .expect_err("not gated");

    assert!(
        matches!(error, DomainError::MetricNotQuotaGated { .. }),
        "{error:?}"
    );
}

// --- replay ahead of mode and size --------------------------------------------

#[tokio::test]
async fn independent_mode_is_not_yet_implemented_but_a_stored_key_answers_first() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let independent = |key: &str| BatchDebitRequest {
        mode: BatchMode::Independent,
        ..batch(vec![item(5, "i1")], key)
    };

    let fresh = h
        .operations()
        .batch_debit(independent("b1"))
        .await
        .expect_err("501");
    let atomic = h
        .operations()
        .batch_debit(batch(vec![item(5, "i1")], "b2"))
        .await
        .expect("atomic");
    assert_eq!(atomic.result, BatchResult::Allowed);
    let stored = h
        .operations()
        .batch_debit(independent("b2"))
        .await
        .expect_err("the stored key is looked up before the mode");

    assert!(
        matches!(fresh, DomainError::NotYetImplemented { .. }),
        "{fresh:?}"
    );
    assert_eq!(stored, DomainError::IdempotencyPayloadMismatch);
}

#[tokio::test]
async fn an_oversized_batch_is_refused_before_any_pdp_call() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;
    h.quota(Some(100)).await;
    let mut operations = h.operations();
    operations.batch.max_items = 1;

    let error = operations
        .batch_debit(batch(vec![item(5, "i1"), item(5, "i2")], "b1"))
        .await
        .expect_err("too large");

    assert_eq!(error, DomainError::BulkTooLarge { items: 2, max: 1 });
    assert_eq!(pdp.calls(), 0);
}

#[tokio::test]
async fn a_lowered_size_limit_also_refuses_the_replay_of_a_larger_batch() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let request = batch(vec![item(5, "i1"), item(5, "i2")], "b1");
    let first = h
        .operations()
        .batch_debit(request.clone())
        .await
        .expect("batch");
    assert_eq!(first.result, BatchResult::Allowed);

    let mut lowered = h.operations();
    lowered.batch.max_items = 1;
    let replay = lowered
        .batch_debit(request)
        .await
        .expect_err("the limit applies before the replay");

    assert_eq!(replay, DomainError::BulkTooLarge { items: 2, max: 1 });
    assert_eq!(h.consumed(id), 10, "the original batch stays applied once");
}

// --- the timeout --------------------------------------------------------------

#[tokio::test]
async fn a_batch_outlasting_its_timeout_writes_nothing_and_leaves_the_key_free() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let request = batch(vec![item(5, "i1"), item(5, "i2")], "b1");

    let mut hurried = h.operations();
    hurried.batch.timeout = Duration::ZERO;
    let error = hurried
        .batch_debit(request.clone())
        .await
        .expect_err("timeout");
    let retried = h.operations().batch_debit(request).await.expect("retry");

    assert_eq!(error, DomainError::BatchTimeout);
    assert_eq!(retried.result, BatchResult::Allowed, "nothing was recorded");
    assert_eq!(h.consumed(id), 10, "only the retry moved the counter");
}

/// Permits the test tenant and records how many evaluations ran at once.
struct OverlappingPdp {
    inner: PermitTenantsPdp,
    running: std::sync::atomic::AtomicUsize,
    most: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for OverlappingPdp {
    async fn evaluate(
        &self,
        ctx: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use std::sync::atomic::Ordering;
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(running, Ordering::SeqCst);
        tokio::task::yield_now().await;
        self.running.fetch_sub(1, Ordering::SeqCst);
        self.inner.evaluate(ctx, request).await
    }
}

#[tokio::test]
async fn the_items_of_a_batch_are_admitted_several_at_a_time_with_one_pdp_call_each() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pdp = Arc::new(OverlappingPdp {
        inner: PermitTenantsPdp::new(vec![tenant().as_uuid()]),
        running: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let h = Harness::with_pdp(pdp.clone()).await;
    h.quota(Some(1_000)).await;
    let items = (0..20).map(|n| item(1, &format!("i{n}"))).collect();

    let decision = h
        .operations()
        .batch_debit(batch(items, "b1"))
        .await
        .expect("batch");

    assert_eq!(allowed(&decision).len(), 20);
    assert_eq!(pdp.inner.calls(), 20);
    let most = pdp.most.load(Ordering::SeqCst);
    assert!(
        (2..=super::ADMISSION_CONCURRENCY).contains(&most),
        "{most} evaluations ran at once"
    );
}

#[tokio::test]
async fn a_batch_item_key_longer_than_the_bound_names_the_item() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;

    let error = h
        .operations()
        .batch_debit(batch(vec![item(1, "i1"), item(1, &"k".repeat(257))], "b1"))
        .await
        .expect_err("a key past the bound");

    assert_eq!(
        error,
        DomainError::InvalidBatchItem {
            index: 1,
            field: "idempotency_key",
            reason: tokens::IDEMPOTENCY_KEY_TOO_LONG,
        }
    );
}
