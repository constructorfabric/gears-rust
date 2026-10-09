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

use super::super::harness_tests::{Harness, attribution};
use crate::domain::error::DomainError;
use crate::domain::ports::metric_registry::MetricMode;
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;
use crate::test_support::{DenyAllPdp, PermitTenantsPdp, ctx, tenant};

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
        .batch_debit(&ctx(), batch(vec![item(30, "i1"), item(40, "i2")], "b1"))
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
async fn a_denied_item_denies_the_batch_moves_nothing_and_is_counted_once() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    // The first item fits; the second fits alone but not after the first.
    let decision = h
        .operations()
        .batch_debit(
            &ctx(),
            batch(vec![item(60, "i1"), item(60, "i2"), item(60, "i3")], "b1"),
        )
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
        .batch_debit(&ctx(), batch(vec![item(150, "i1"), item(90, "i2")], "b1"))
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
        .batch_debit(&ctx(), request.clone())
        .await
        .expect("batch");

    let replica = Harness::new_over(&h).await;
    let again = replica
        .operations()
        .batch_debit(&ctx(), request)
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
async fn a_key_reused_for_a_different_batch_is_a_payload_mismatch() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let stored = h
        .operations()
        .batch_debit(&ctx(), batch(vec![item(10, "i1")], "b1"))
        .await
        .expect("batch");
    assert_eq!(stored.result, BatchResult::Allowed);

    let error = h
        .operations()
        .batch_debit(&ctx(), batch(vec![item(11, "i1")], "b1"))
        .await
        .expect_err("mismatch");

    assert_eq!(error, DomainError::IdempotencyPayloadMismatch);
}

// --- fail-fast order ----------------------------------------------------------

#[tokio::test]
async fn an_empty_batch_is_refused_before_its_key_is_looked_at() {
    let h = Harness::new().await;

    let error = h
        .operations()
        .batch_debit(&ctx(), batch(Vec::new(), " "))
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
        .batch_debit(&ctx(), batch(vec![item(5, "i1"), item(0, "i2")], ""))
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
        .batch_debit(&ctx(), batch(vec![item(5, " ")], " "))
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
        .batch_debit(&ctx(), batch(vec![item(5, "i1"), item(5, "")], "b1"))
        .await
        .expect_err("blank");
    let duplicate = ops
        .batch_debit(&ctx(), batch(vec![item(5, "i1"), item(5, "i1")], "b1"))
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
        .batch_debit(&ctx(), batch(vec![item(5, "i1"), other], "b1"))
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
        .batch_debit(&ctx(), batch(vec![item(5, "i1")], "b1"))
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
        .batch_debit(&ctx(), batch(vec![item(5, "i1")], "b1"))
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
        .batch_debit(&ctx(), independent("b1"))
        .await
        .expect_err("501");
    let atomic = h
        .operations()
        .batch_debit(&ctx(), batch(vec![item(5, "i1")], "b2"))
        .await
        .expect("atomic");
    assert_eq!(atomic.result, BatchResult::Allowed);
    let stored = h
        .operations()
        .batch_debit(&ctx(), independent("b2"))
        .await
        .expect_err("the stored key is looked up before the mode");

    assert!(
        matches!(fresh, DomainError::NotYetImplemented { .. }),
        "{fresh:?}"
    );
    assert_eq!(stored, DomainError::IdempotencyPayloadMismatch);
}

#[tokio::test]
async fn a_stored_batch_replays_after_the_size_limit_was_lowered() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let request = batch(vec![item(5, "i1"), item(5, "i2")], "b1");
    let first = h
        .operations()
        .batch_debit(&ctx(), request.clone())
        .await
        .expect("batch");

    let mut lowered = h.operations();
    lowered.batch.max_items = 1;
    let replay = lowered.batch_debit(&ctx(), request).await.expect("replay");
    let fresh = lowered
        .batch_debit(&ctx(), batch(vec![item(5, "i1"), item(5, "i2")], "b2"))
        .await
        .expect_err("too large");

    assert_eq!(replay, first);
    assert_eq!(
        fresh,
        DomainError::BulkTooLarge { items: 2, max: 1 },
        "a new key meets the lowered limit"
    );
    assert_eq!(h.consumed(id), 10);
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
        .batch_debit(&ctx(), request.clone())
        .await
        .expect_err("timeout");
    let retried = h
        .operations()
        .batch_debit(&ctx(), request)
        .await
        .expect("retry");

    assert_eq!(error, DomainError::BatchTimeout);
    assert_eq!(retried.result, BatchResult::Allowed, "nothing was recorded");
    assert_eq!(h.consumed(id), 10, "only the retry moved the counter");
}
