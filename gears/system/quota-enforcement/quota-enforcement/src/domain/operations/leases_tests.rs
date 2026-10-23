#![allow(clippy::expect_used)]
//! The three lease operations over real engines and the in-memory double:
//! fail-fast order, holds and their settlement, replay, the cap, and the
//! telemetry each path leaves behind.

use std::sync::Arc;

use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, DecisionResult, LeaseState,
    LeaseToken, ReleaseLeaseRequest, RollbackRequest, RollbackableOperation, TenantId,
};

use super::super::harness_tests::{Harness, attribution};
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::ports::metrics::{DenialReason, OperationKind};
use crate::domain::tokens;
use crate::test_support::{DenyAllPdp, METRIC_TOKENS, ctx, tenant};

fn acquire(amount: i64, ttl_secs: Option<u64>, key: &str) -> AcquireLeaseRequest {
    AcquireLeaseRequest {
        attribution: attribution(),
        amount,
        ttl_secs,
        idempotency_key: key.to_owned(),
    }
}

fn commit(token: LeaseToken, actual_amount: Option<i64>, key: &str) -> CommitLeaseRequest {
    CommitLeaseRequest {
        tenant_id: tenant(),
        token,
        actual_amount,
        idempotency_key: key.to_owned(),
    }
}

fn release(token: LeaseToken, key: &str) -> ReleaseLeaseRequest {
    ReleaseLeaseRequest {
        tenant_id: tenant(),
        token,
        idempotency_key: key.to_owned(),
    }
}

/// Acquire and return the token, failing the test on anything else.
async fn acquired(h: &Harness, amount: i64, key: &str) -> LeaseToken {
    match h
        .operations()
        .acquire_lease(&ctx(), acquire(amount, Some(60), key))
        .await
        .expect("acquire")
    {
        AcquireLeaseOutcome::Acquired { token, .. } => token,
        AcquireLeaseOutcome::Denied { decision } => panic!("denied: {decision:?}"),
    }
}

fn invalid(error: &DomainError, field: &str, reason: &str) -> bool {
    matches!(
        error,
        DomainError::InvalidArgument { field: f, reason: r } if *f == field && *r == reason
    )
}

// --- acquire ------------------------------------------------------------------

#[tokio::test]
async fn an_allowed_acquisition_holds_the_amount_until_it_is_settled() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;

    let token = acquired(&h, 30, "a1").await;

    assert_eq!(h.consumed(id), 30, "the hold occupies capacity");
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Active));
    assert_eq!(h.metrics.evaluations(), vec![OperationKind::Reserve]);
    assert_eq!(
        h.metrics.lease_waits(),
        vec![METRIC_TOKENS.to_owned()],
        "the acquisition wait carries the admitted metric"
    );
}

#[tokio::test]
async fn a_denied_acquisition_is_a_verdict_that_holds_nothing() {
    let h = Harness::new().await;
    let id = h.quota(Some(5)).await;

    let outcome = h
        .operations()
        .acquire_lease(&ctx(), acquire(50, Some(60), "a1"))
        .await
        .expect("a denial is not an error");

    let AcquireLeaseOutcome::Denied { decision } = outcome else {
        panic!("expected a denial, got {outcome:?}");
    };
    assert!(matches!(decision.result, DecisionResult::Denied { .. }));
    assert_eq!(h.consumed(id), 0);
    assert!(h.metrics.denials().contains(&DenialReason::QuotaExceeded));
}

#[tokio::test]
async fn amount_and_ttl_are_refused_before_the_pdp_and_storage() {
    let h = Harness::with_pdp(Arc::new(DenyAllPdp)).await;

    for amount in [0, -1] {
        let error = h
            .operations()
            .acquire_lease(&ctx(), acquire(amount, Some(60), "a1"))
            .await
            .expect_err("a non-positive amount cannot be held");
        assert!(
            invalid(&error, "amount", tokens::INVALID_AMOUNT),
            "got {error:?}"
        );
    }
    // Missing, one below the window, one above it: refused, never clamped.
    for ttl in [None, Some(0), Some(3_601)] {
        let error = h
            .operations()
            .acquire_lease(&ctx(), acquire(10, ttl, "a1"))
            .await
            .expect_err("a TTL outside the window is refused");
        assert!(
            invalid(&error, "ttl", tokens::TTL_OUT_OF_BOUNDS),
            "ttl {ttl:?}: {error:?}"
        );
    }
    // A denying PDP never answered: every refusal came first.
    assert!(
        h.metrics
            .denials()
            .iter()
            .all(|reason| *reason == DenialReason::InvalidArgument)
    );
}

#[tokio::test]
async fn the_window_bounds_are_themselves_accepted() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    for (ttl, key) in [(1, "min"), (3_600, "max")] {
        let outcome = h
            .operations()
            .acquire_lease(&ctx(), acquire(1, Some(ttl), key))
            .await
            .expect("inside the window");
        assert!(matches!(outcome, AcquireLeaseOutcome::Acquired { .. }));
    }
}

#[tokio::test]
async fn a_replayed_acquisition_returns_the_original_token_and_holds_once() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let first = acquired(&h, 20, "a1").await;

    // A second replica: its answer has to come from the stored record.
    let replica = Harness::new_over(&h).await;
    let again = replica
        .operations()
        .acquire_lease(&ctx(), acquire(20, Some(60), "a1"))
        .await
        .expect("replay");

    assert!(
        matches!(again, AcquireLeaseOutcome::Acquired { token, .. } if token == first),
        "got {again:?}"
    );
    assert_eq!(h.consumed(id), 20, "held once");
    assert_eq!(replica.metrics.replays(), vec![OperationKind::Reserve]);

    let error = replica
        .operations()
        .acquire_lease(&ctx(), acquire(21, Some(60), "a1"))
        .await
        .expect_err("same key, different request");
    assert!(matches!(error, DomainError::IdempotencyPayloadMismatch));
}

#[tokio::test]
async fn the_active_lease_cap_refuses_with_its_counter_and_a_denial_does_not_hit_it() {
    let h = Harness::with_lease_cap(1).await;
    h.quota(Some(10)).await;
    acquired(&h, 5, "a1").await;

    let error = h
        .operations()
        .acquire_lease(&ctx(), acquire(1, Some(60), "a2"))
        .await
        .expect_err("the cap is full");
    assert!(matches!(error, DomainError::LeaseInflightLimitExceeded));
    assert_eq!(h.metrics.lease_cap(), vec![METRIC_TOKENS.to_owned()]);

    // An acquisition the policy refuses is a verdict, cap or no cap.
    let denied = h
        .operations()
        .acquire_lease(&ctx(), acquire(50, Some(60), "a3"))
        .await
        .expect("a denial is not an error");
    assert!(matches!(denied, AcquireLeaseOutcome::Denied { .. }));
}

#[tokio::test]
async fn an_expired_lease_frees_its_capacity_and_its_cap_slot_without_a_sweep() {
    let h = Harness::with_lease_cap(1).await;
    let id = h.quota(Some(10)).await;
    acquired(&h, 10, "a1").await;
    h.storage.expire_leases();

    let token = acquired(&h, 10, "a2").await;
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Active));
    assert_eq!(h.consumed(id), 10, "only the live hold counts");
}

// --- commit -------------------------------------------------------------------

#[tokio::test]
async fn a_commit_keeps_what_was_used_and_returns_the_rest() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let token = acquired(&h, 30, "a1").await;

    let decision = h
        .operations()
        .commit_lease(&ctx(), commit(token, Some(12), "c1"))
        .await
        .expect("commit");

    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed(id), 12);
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Committed));
    assert_eq!(
        h.metrics.evaluations(),
        vec![OperationKind::Reserve, OperationKind::Commit]
    );

    // The same key replays; nothing moves twice.
    h.operations()
        .commit_lease(&ctx(), commit(token, Some(12), "c1"))
        .await
        .expect("replay");
    assert_eq!(h.consumed(id), 12);
    assert_eq!(h.metrics.replays(), vec![OperationKind::Commit]);
}

#[tokio::test]
async fn a_zero_commit_returns_every_hold_and_a_negative_one_is_refused_first() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let token = acquired(&h, 30, "a1").await;

    let error = h
        .operations()
        .commit_lease(&ctx(), commit(token, Some(-1), "c1"))
        .await
        .expect_err("a negative amount is refused");
    assert!(
        invalid(&error, "actual_amount", tokens::INVALID_AMOUNT),
        "got {error:?}"
    );
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Active));

    h.operations()
        .commit_lease(&ctx(), commit(token, Some(0), "c2"))
        .await
        .expect("a zero commit");
    assert_eq!(h.consumed(id), 0, "everything went back");
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Committed));
}

#[tokio::test]
async fn committing_more_than_was_reserved_is_refused() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let token = acquired(&h, 10, "a1").await;

    let error = h
        .operations()
        .commit_lease(&ctx(), commit(token, Some(11), "c1"))
        .await
        .expect_err("over-commit");
    assert!(
        matches!(error, DomainError::OverCommitNotAuthorized { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn a_committed_lease_rolls_back_only_when_the_rollback_names_a_lease_commit() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let token = acquired(&h, 30, "a1").await;
    h.operations()
        .commit_lease(&ctx(), commit(token, Some(12), "shared"))
        .await
        .expect("commit");

    // Under the default, the key names a direct debit, and there is none.
    let error = h
        .operations()
        .rollback(
            &ctx(),
            RollbackRequest {
                attribution: attribution(),
                original_operation: RollbackableOperation::Debit,
                original_idempotency_key: "shared".to_owned(),
                idempotency_key: "r1".to_owned(),
            },
        )
        .await
        .expect_err("no debit under that key");
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
    assert_eq!(h.consumed(id), 12);

    h.operations()
        .rollback(
            &ctx(),
            RollbackRequest {
                attribution: attribution(),
                original_operation: RollbackableOperation::LeaseCommit,
                original_idempotency_key: "shared".to_owned(),
                idempotency_key: "r2".to_owned(),
            },
        )
        .await
        .expect("the lease commit is reversed");
    assert_eq!(h.consumed(id), 0);
}

// --- release ------------------------------------------------------------------

#[tokio::test]
async fn a_release_returns_everything_and_a_settled_lease_is_not_active() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let token = acquired(&h, 30, "a1").await;

    h.operations()
        .release_lease(&ctx(), release(token, "r1"))
        .await
        .expect("release");
    assert_eq!(h.consumed(id), 0);
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Released));
    assert_eq!(
        h.metrics.evaluations(),
        vec![OperationKind::Reserve, OperationKind::Release]
    );

    // The same key replays; another key finds nothing left to release.
    h.operations()
        .release_lease(&ctx(), release(token, "r1"))
        .await
        .expect("replay");
    let error = h
        .operations()
        .release_lease(&ctx(), release(token, "r2"))
        .await
        .expect_err("already released");
    assert!(
        matches!(error, DomainError::LeaseNotActive { .. }),
        "got {error:?}"
    );
    let error = h
        .operations()
        .commit_lease(&ctx(), commit(token, None, "c1"))
        .await
        .expect_err("already released");
    assert!(
        matches!(error, DomainError::LeaseNotActive { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn an_expired_lease_cannot_be_settled() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let token = acquired(&h, 10, "a1").await;
    h.storage.expire_leases();

    let error = h
        .operations()
        .commit_lease(&ctx(), commit(token, None, "c1"))
        .await
        .expect_err("expired");
    assert!(
        matches!(error, DomainError::LeaseNotActive { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn an_unknown_token_or_another_tenants_lease_is_not_found() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let token = acquired(&h, 10, "a1").await;

    let error = h
        .operations()
        .release_lease(&ctx(), release(LeaseToken::new(uuid::Uuid::now_v7()), "r1"))
        .await
        .expect_err("unknown token");
    assert!(
        matches!(
            error,
            DomainError::NotFound {
                kind: ResourceKind::Lease,
                ..
            }
        ),
        "got {error:?}"
    );

    // Another tenant's request for this token: the lookup runs inside the
    // requested tenant, which does not hold it, so it is not found there.
    let error = h
        .operations()
        .release_lease(
            &ctx(),
            ReleaseLeaseRequest {
                tenant_id: TenantId::new(uuid::Uuid::now_v7()),
                token,
                idempotency_key: "r2".to_owned(),
            },
        )
        .await
        .expect_err("foreign tenant");
    assert!(
        matches!(
            error,
            DomainError::NotFound {
                kind: ResourceKind::Lease,
                ..
            }
        ),
        "got {error:?}"
    );
    assert_eq!(h.storage.lease_state(token), Some(LeaseState::Active));
}
