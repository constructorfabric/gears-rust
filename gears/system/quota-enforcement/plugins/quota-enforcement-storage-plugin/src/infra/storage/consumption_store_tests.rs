#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, Decision, DecisionResult, EvaluatedMutation,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, NO_APPLICABLE_QUOTA,
    NotificationEventKind, OperationType, PartialIdempotencyWrite, PayloadHash, PeriodType,
    PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft, QuotaId, QuotaType, Retention,
    RollbackTarget, StorageError, TransitionOutcome,
};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::Db;
use toolkit_db::outbox::OutboxHandle;
use toolkit_security::SecurityContext;

use super::SqlConsumptionStore;
use crate::domain::ports::{ConsumptionStore, LeaseStore, QuotaStore};
use crate::infra::storage::{SqlPolicyStore, SqlQuotaStore};
use crate::test_support::{
    METRIC_TOKENS, actor, bound_outbox, draft, enqueued_messages, scope_for, tenant, test_db, user,
};

/// A Tuesday, inside an ordinary day period.
const DAY_ONE: OffsetDateTime = datetime!(2026-03-17 10:00:00 UTC);

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("context")
}

fn authorized() -> AttributionDigest {
    AttributionDigest::from_bytes([7; 32])
}

fn applicable(subject_id: &str) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects: vec![user(subject_id)],
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

fn write(op: OperationType, key: &str, payload: u8) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&[user("u1")]),
            operation_type: op,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

fn partial(key: &str, payload: u8) -> PartialIdempotencyWrite {
    PartialIdempotencyWrite {
        tenant_id: tenant(),
        key: key.to_owned(),
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(50).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

/// An evaluator that answers from a script instead of running an engine, and
/// counts how often the transaction called it.
#[derive(Default)]
struct Canned {
    calls: Mutex<u32>,
}

impl Canned {
    /// Allow the whole amount against every applicable Quota whose remaining
    /// capacity covers it; deny when one does not, and report no applicable
    /// Quota when the set is empty.
    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
    ) -> Result<EvaluationOutcome, quota_enforcement_sdk::engine::EvaluationFailure> {
        *self.calls.lock().expect("lock") += 1;
        if context.quotas.is_empty() {
            return Ok(EvaluationOutcome::validate(
                Decision {
                    result: DecisionResult::Denied {
                        violated_quota_ids: Vec::new(),
                        reason: NO_APPLICABLE_QUOTA.to_owned(),
                    },
                    debit_plan: std::collections::BTreeMap::new(),
                    diagnostics: std::collections::BTreeMap::new(),
                },
                context,
            )?);
        }
        let exceeded: Vec<QuotaId> = context
            .quotas
            .iter()
            .filter(|quota| {
                quota
                    .snapshot
                    .remaining
                    .is_some_and(|remaining| remaining < context.amount)
            })
            .map(|quota| quota.snapshot.quota_id)
            .collect();
        let decision = if exceeded.is_empty() {
            Decision::allowed_with_plan(
                context
                    .quotas
                    .iter()
                    .map(|quota| {
                        (
                            quota.snapshot.quota_id,
                            QuotaDebitPlan {
                                amount: context.amount,
                            },
                        )
                    })
                    .collect(),
            )
        } else {
            Decision {
                result: DecisionResult::Denied {
                    violated_quota_ids: exceeded,
                    reason: "QUOTA_EXCEEDED".to_owned(),
                },
                debit_plan: std::collections::BTreeMap::new(),
                diagnostics: std::collections::BTreeMap::new(),
            }
        };
        Ok(EvaluationOutcome::validate(decision, context)?)
    }
}

struct Harness {
    db: Db,
    store: SqlConsumptionStore,
    quotas: SqlQuotaStore,
    outbox: OutboxHandle,
    clock: Arc<Mutex<OffsetDateTime>>,
    evaluator: Arc<Canned>,
}

impl Harness {
    async fn up() -> Self {
        let db = test_db().await;
        let (outbox, enqueuer) = bound_outbox(&db).await;
        let clock = Arc::new(Mutex::new(DAY_ONE));
        let reader = Arc::clone(&clock);
        let store = SqlConsumptionStore::with_clock(
            db.clone(),
            Arc::clone(&enqueuer) as Arc<dyn crate::infra::outbox::NotificationEnqueuer>,
            Arc::new(move || *reader.lock().expect("clock")),
        );
        let policies = SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer) as _);
        let quota_clock = Arc::clone(&clock);
        policies
            .create_policy(
                &ctx(),
                PolicyDraft {
                    scope: PolicyScope::Global,
                    engine_id: "most-restrictive-wins".into(),
                    engine_config: serde_json::json!({}),
                    timeout_ms: Some(25),
                    description: None,
                    comment: None,
                    created_by: "test".into(),
                    schema_snapshot: PolicySchemaSnapshot::default(),
                },
                &[],
            )
            .await
            .expect("seed the global policy");
        let quotas = SqlQuotaStore::with_clock(
            db.clone(),
            enqueuer,
            Arc::new(move || *quota_clock.lock().expect("clock")),
        );
        Self {
            db,
            store,
            quotas,
            outbox,
            clock,
            evaluator: Arc::new(Canned::default()),
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    fn set_now(&self, now: OffsetDateTime) {
        *self.clock.lock().expect("clock") = now;
    }

    fn callback(&self) -> Arc<TransactionEvaluator> {
        let evaluator = Arc::clone(&self.evaluator);
        Arc::new(move |context: &EvaluationContext<'_>| evaluator.evaluate(context))
    }

    fn calls(&self) -> u32 {
        *self.evaluator.calls.lock().expect("lock")
    }

    async fn consumption_quota(
        &self,
        subject_id: &str,
        cap: Option<u64>,
        thresholds: Vec<u8>,
    ) -> QuotaId {
        let mut d: QuotaDraft = draft(tenant(), subject_id, cap);
        d.quota_type = QuotaType::Consumption;
        d.period = Some(PeriodType::Day);
        d.notification_thresholds = thresholds;
        self.quotas
            .create_quota(&actor(), &scope_for(tenant()), d, &[])
            .await
            .expect("create quota")
    }

    async fn allocation_quota(&self, subject_id: &str, cap: Option<u64>) -> QuotaId {
        self.quotas
            .create_quota(
                &actor(),
                &scope_for(tenant()),
                draft(tenant(), subject_id, cap),
                &[],
            )
            .await
            .expect("create quota")
    }

    async fn debit(
        &self,
        subject_id: &str,
        amount: u64,
        idempotency: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>, StorageError> {
        let applicable = applicable(subject_id);
        let null = serde_json::Value::Null;
        self.store
            .apply_debit_plan(
                &ctx(),
                &scope_for(tenant()),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &null,
                    resource: &null,
                    user_projection: None,
                    limits: limits(),
                    idempotency,
                    authorized: authorized(),
                    evaluate: self.callback(),
                },
                &[],
            )
            .await
    }

    async fn acquire(
        &self,
        subject_id: &str,
        amount: u64,
        ttl: std::time::Duration,
        idempotency: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedLease>, StorageError> {
        let applicable = applicable(subject_id);
        let null = serde_json::Value::Null;
        self.store
            .acquire_lease(
                &ctx(),
                &scope_for(tenant()),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &null,
                    resource: &null,
                    user_projection: None,
                    limits: limits(),
                    idempotency,
                    authorized: authorized(),
                    evaluate: self.callback(),
                },
                ttl,
            )
            .await
    }

    async fn consumed(&self, subject_id: &str, id: QuotaId) -> u64 {
        self.store
            .read_quota_snapshot(&ctx(), &scope_for(tenant()), &applicable(subject_id))
            .await
            .expect("snapshot")
            .into_iter()
            .find(|snapshot| snapshot.quota_id == id)
            .map_or(0, |snapshot| snapshot.consumed)
    }

    async fn events(&self) -> Vec<String> {
        enqueued_messages(&self.db)
            .await
            .into_iter()
            .map(|message| message.payload_type)
            .collect()
    }
}

// --- debit and replay -------------------------------------------------------

#[tokio::test]
async fn a_first_debit_materializes_the_period_and_records_its_attribution() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), vec![50, 80]).await;

    let applied = h
        .debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    assert!(matches!(applied, TransitionOutcome::Applied(_)));
    let debit = applied.get();
    assert_eq!(debit.mutation.counters[0].value, 10);
    assert!(debit.mutation.counters[0].period_id.is_some());
    assert!(matches!(debit.retention, Retention::Recorded { .. }));
    assert_eq!(h.consumed("u1", id).await, 10);

    let record = h
        .store
        .lookup_idempotency(&write(OperationType::Debit, "k1", 1).scope)
        .await
        .expect("lookup")
        .expect("the debit recorded its outcome");
    assert_eq!(record.attribution_hash, Some(authorized()));
    assert_eq!(record.decision_blob["__version"], serde_json::json!(1));
    h.down().await;
}

#[tokio::test]
async fn a_replay_returns_the_stored_decision_without_evaluating_again() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    let key = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 10, &key).await.expect("first debit");
    let calls = h.calls();

    let replay = h.debit("u1", 10, &key).await.expect("replay");

    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert_eq!(h.calls(), calls, "the engine is never re-invoked");
    assert!(
        replay.get().mutation.counters.is_empty(),
        "a replay moved nothing, so it reports no counter movement"
    );
    assert_eq!(h.consumed("u1", id).await, 10);
    h.down().await;
}

#[tokio::test]
async fn a_divergent_payload_under_the_same_key_is_refused_and_changes_nothing() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("first debit");

    let err = h
        .debit("u1", 99, &write(OperationType::Debit, "k1", 2))
        .await
        .expect_err("the same key carried a different payload");

    assert_eq!(err, StorageError::IdempotencyPayloadMismatch);
    assert_eq!(h.consumed("u1", id).await, 10);
    h.down().await;
}

#[tokio::test]
async fn a_denial_occupies_its_key_and_moves_no_counter() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(10), Vec::new()).await;
    let key = write(OperationType::Debit, "k1", 1);

    let denied = h
        .debit("u1", 50, &key)
        .await
        .expect("a denial is a success");

    assert_eq!(
        denied.get().decision.denied_reason(),
        Some("QUOTA_EXCEEDED")
    );
    assert!(matches!(denied.get().retention, Retention::Recorded { .. }));
    assert_eq!(h.consumed("u1", id).await, 0);
    let calls = h.calls();
    let replay = h.debit("u1", 50, &key).await.expect("replay of a denial");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert_eq!(h.calls(), calls);
    h.down().await;
}

#[tokio::test]
async fn a_no_applicable_quota_denial_persists_nothing_and_is_reevaluated() {
    let h = Harness::up().await;
    let key = write(OperationType::Debit, "k1", 1);

    let denied = h.debit("u1", 5, &key).await.expect("denial");

    assert_eq!(
        denied.get().decision.denied_reason(),
        Some(NO_APPLICABLE_QUOTA)
    );
    assert_eq!(denied.get().retention, Retention::Unrecorded);
    assert!(
        h.store
            .lookup_idempotency(&key.scope)
            .await
            .expect("lookup")
            .is_none()
    );

    // Provisioning a Quota must change the answer for the very same request.
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    let allowed = h.debit("u1", 5, &key).await.expect("re-evaluated");
    assert_eq!(allowed.get().decision.result, DecisionResult::Allowed);
    assert_eq!(h.consumed("u1", id).await, 5);
    h.down().await;
}

// --- periods and thresholds -------------------------------------------------

#[tokio::test]
async fn a_debit_at_exactly_the_boundary_settles_the_closing_row_once() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("first debit");

    // Exactly at period_end: half-open windows put this in the next period.
    h.set_now(datetime!(2026-03-18 00:00:00 UTC));
    h.debit("u1", 4, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit after the boundary");

    assert_eq!(h.consumed("u1", id).await, 4, "the new period starts fresh");
    let rollovers = h
        .events()
        .await
        .into_iter()
        .filter(|kind| kind == NotificationEventKind::PeriodRollover.as_str())
        .count();
    assert_eq!(rollovers, 1, "exactly one rollover per closing row");
    h.down().await;
}

#[tokio::test]
async fn thresholds_fire_upward_once_and_a_credit_does_not_rearm_them() {
    let h = Harness::up().await;
    let id = h
        .consumption_quota("u1", Some(100), vec![50, 80, 100])
        .await;

    h.debit("u1", 85, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit past two thresholds");
    let crossed = h
        .events()
        .await
        .into_iter()
        .filter(|kind| kind == NotificationEventKind::ThresholdCrossed.as_str())
        .count();
    assert_eq!(crossed, 1, "one event carries every threshold crossed");

    h.store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 40, &partial("c1", 9), &[])
        .await
        .expect("credit");
    h.debit("u1", 40, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit back across 80");

    let crossed_after = h
        .events()
        .await
        .into_iter()
        .filter(|kind| kind == NotificationEventKind::ThresholdCrossed.as_str())
        .count();
    assert_eq!(
        crossed_after, 1,
        "a threshold crossed once stays crossed for the period"
    );
    h.down().await;
}

#[tokio::test]
async fn a_snapshot_read_materializes_the_current_row_and_settles_nothing() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let before = h.events().await.len();

    h.set_now(datetime!(2026-03-19 00:00:00 UTC));
    assert_eq!(h.consumed("u1", id).await, 0, "the new period starts empty");
    assert_eq!(
        h.events().await.len(),
        before,
        "a read enqueues nothing, so a preview never writes an outbox row"
    );

    // The next mutation is what settles and emits.
    h.debit("u1", 1, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit");
    assert!(
        h.events()
            .await
            .iter()
            .any(|kind| kind == NotificationEventKind::PeriodRollover.as_str())
    );
    h.down().await;
}

// --- credit -----------------------------------------------------------------

#[tokio::test]
async fn a_credit_lowers_the_counter_to_the_floor_and_emits_one_adjustment() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    let applied = h
        .store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 40, &partial("c1", 9), &[])
        .await
        .expect("credit");

    assert!(matches!(applied, TransitionOutcome::Applied(_)));
    assert_eq!(h.consumed("u1", id).await, 0, "a credit floors at zero");
    assert!(
        h.events()
            .await
            .iter()
            .any(|kind| kind == NotificationEventKind::QuotaCounterAdjusted.as_str())
    );

    // The scope the plugin derived is the Quota's own subject pair.
    let derived = IdempotencyScope {
        tenant_id: tenant(),
        subject_key: IdempotencySubjectKey::of(&[user("u1")]),
        operation_type: OperationType::Credit,
        key: "c1".to_owned(),
    };
    assert!(
        h.store
            .lookup_idempotency(&derived)
            .await
            .expect("lookup")
            .is_some()
    );
    h.down().await;
}

#[tokio::test]
async fn an_unknown_quota_is_refused_before_anything_is_written() {
    let h = Harness::up().await;
    let missing = QuotaId::generate();

    let err = h
        .store
        .apply_credit(
            &ctx(),
            &scope_for(tenant()),
            missing,
            1,
            &partial("c1", 9),
            &[],
        )
        .await
        .expect_err("no such quota");

    assert_eq!(err, StorageError::QuotaNotFound { id: missing });
    h.down().await;
}

#[tokio::test]
async fn a_credit_replays_after_its_quota_was_deactivated() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    h.store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 4, &partial("c1", 9), &[])
        .await
        .expect("credit");
    h.quotas
        .deactivate_quota(&actor(), &scope_for(tenant()), id, &[])
        .await
        .expect("deactivate");

    let replay = h
        .store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 4, &partial("c1", 9), &[])
        .await
        .expect("a replay answers from the record, not from the guards");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));

    let fresh = h
        .store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 1, &partial("c2", 8), &[])
        .await
        .expect_err("a fresh credit is refused");
    assert_eq!(fresh, StorageError::QuotaDeactivated { id });
    h.down().await;
}

#[tokio::test]
async fn a_credit_refuses_a_closed_period_but_opens_one_never_evaluated() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;

    // Never evaluated: the current window is open, so the row is materialized.
    h.store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 5, &partial("c1", 9), &[])
        .await
        .expect("a quota with no row has an open current window");

    h.set_now(datetime!(2026-03-19 00:00:00 UTC));
    let err = h
        .store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 5, &partial("c2", 8), &[])
        .await
        .expect_err("the latest period has ended");
    assert_eq!(err, StorageError::PeriodClosed);
    h.down().await;
}

// --- rollback ---------------------------------------------------------------

#[tokio::test]
async fn a_rollback_reverses_the_acquisition_period_after_the_boundary() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    let original = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 30, &original).await.expect("debit");

    // Cross into the next period, then reverse: the closing row is the one that
    // must lose the amount, not the row now current.
    h.set_now(datetime!(2026-03-18 00:00:00 UTC));
    h.store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &RollbackTarget {
                original: original.scope.clone(),
                authorized: authorized(),
            },
            &write(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect("rollback into the settlement window");

    assert!(
        h.events()
            .await
            .iter()
            .any(|kind| kind == NotificationEventKind::QuotaRollbackApplied.as_str())
    );
    assert_eq!(h.consumed("u1", id).await, 0, "the new period is untouched");
    h.down().await;
}

#[tokio::test]
async fn a_rollback_authorized_under_another_attribution_finds_nothing() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    let original = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 30, &original).await.expect("debit");

    let err = h
        .store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &RollbackTarget {
                original: original.scope.clone(),
                authorized: AttributionDigest::from_bytes([9; 32]),
            },
            &write(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect_err("the caller was admitted for something else");

    assert!(matches!(err, StorageError::OperationNotFound { .. }));
    assert_eq!(h.consumed("u1", id).await, 30, "no counter was touched");
    h.down().await;
}

#[tokio::test]
async fn a_rollback_of_an_unknown_or_denied_operation_finds_nothing() {
    let h = Harness::up().await;
    h.consumption_quota("u1", Some(10), Vec::new()).await;
    let denied = write(OperationType::Debit, "denied", 4);
    h.debit("u1", 99, &denied).await.expect("a denial succeeds");

    for (label, original) in [
        (
            "an unknown key",
            write(OperationType::Debit, "nope", 1).scope,
        ),
        ("a denied debit", denied.scope.clone()),
    ] {
        let err = h
            .store
            .apply_rollback(
                &ctx(),
                &scope_for(tenant()),
                &RollbackTarget {
                    original,
                    authorized: authorized(),
                },
                &write(OperationType::Rollback, label, 2),
                &[],
            )
            .await
            .expect_err("only a committed debit is reversible");
        assert!(
            matches!(err, StorageError::OperationNotFound { .. }),
            "{label} should not be reversible, got {err:?}"
        );
    }
    h.down().await;
}

#[tokio::test]
async fn a_second_rollback_under_another_key_reverses_nothing_further() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    let original = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 30, &original).await.expect("debit");
    let target = RollbackTarget {
        original: original.scope.clone(),
        authorized: authorized(),
    };
    h.store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &target,
            &write(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect("first rollback");
    assert_eq!(h.consumed("u1", id).await, 0);

    let again = h
        .store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &target,
            &write(OperationType::Rollback, "r2", 3),
            &[],
        )
        .await
        .expect("a second rollback is accepted");

    assert!(
        again.get().decision.debit_plan.is_empty(),
        "reversal happens once, so the second plan is empty"
    );
    assert_eq!(h.consumed("u1", id).await, 0);
    h.down().await;
}

// --- retention --------------------------------------------------------------

#[tokio::test]
async fn a_replay_after_the_retention_window_is_a_new_operation() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    let key = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 10, &key).await.expect("debit");

    let reclaimed = h
        .store
        .reclaim_expired_idempotency(10, datetime!(2999-01-01 00:00:00 UTC))
        .await
        .expect("reclaim");
    assert_eq!(reclaimed, 1);

    h.debit("u1", 10, &key)
        .await
        .expect("re-evaluated as a new operation");
    assert_eq!(h.consumed("u1", id).await, 20);
    h.down().await;
}

#[tokio::test]
async fn a_record_that_replaced_an_expired_one_survives_the_sweep_that_selected_its_key() {
    use crate::infra::storage::repo::idempotency_repo::{self as idem_repo, ScopeKey};
    let h = Harness::up().await;
    h.consumption_quota("u1", Some(100), Vec::new()).await;
    let key = write(OperationType::Debit, "k1", 1);
    let first = h.debit("u1", 10, &key).await.expect("debit");
    let Retention::Recorded {
        expires_at: first_expiry,
    } = first.get().retention
    else {
        panic!("the debit is recorded");
    };

    // A sweep selects the key once its record has expired...
    let sweep_at = first_expiry + time::Duration::seconds(1);
    let all = toolkit_security::AccessScope::allow_all();
    let conn = h.db.conn().expect("conn");
    let doomed = idem_repo::select_expired(&conn, &all, 10, sweep_at)
        .await
        .expect("select");
    assert_eq!(doomed.len(), 1);

    // ...and before it deletes, a debit under the same key replaces the record.
    h.set_now(sweep_at);
    let second = h.debit("u1", 10, &key).await.expect("a new operation");
    assert!(matches!(second, TransitionOutcome::Applied(_)));

    let (tenant_id, subject_key, operation_type, idem_key) = &doomed[0];
    let deleted = idem_repo::delete_if_expired(
        &conn,
        &all,
        &ScopeKey {
            tenant_id: *tenant_id,
            subject_key,
            operation_type,
            idem_key,
        },
        sweep_at,
    )
    .await
    .expect("delete");
    assert_eq!(deleted, 0, "the replacement has not expired");

    // So the replacement still answers its replay instead of debiting again.
    let replay = h.debit("u1", 10, &key).await.expect("replay");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    h.down().await;
}

#[tokio::test]
async fn the_operation_log_is_reclaimed_in_batches() {
    let h = Harness::up().await;
    h.consumption_quota("u1", Some(1000), Vec::new()).await;
    for (index, key) in ["k1", "k2", "k3"].into_iter().enumerate() {
        h.debit(
            "u1",
            1,
            &write(
                OperationType::Debit,
                key,
                u8::try_from(index).expect("small"),
            ),
        )
        .await
        .expect("debit");
    }
    let far_future = datetime!(2999-01-01 00:00:00 UTC);

    assert_eq!(
        h.store
            .reclaim_operation_log(2, far_future)
            .await
            .expect("first batch"),
        2
    );
    let rest = h
        .store
        .reclaim_operation_log(10, far_future)
        .await
        .expect("remaining");
    assert!(
        rest >= 1,
        "the quota creation row and the last debit remain"
    );
    h.down().await;
}

#[tokio::test]
async fn a_cap_cannot_be_lowered_below_what_the_current_period_consumed() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 60, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    let refused = h
        .quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(50)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("the cap guard reads the period counter (I6)");

    assert!(
        format!("{refused:?}").contains("CapBelowConsumed"),
        "got {refused:?}"
    );

    // A cap at or above the consumed amount still applies.
    h.quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(60)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("a cap equal to the consumed amount is allowed");
    h.down().await;
}

#[tokio::test]
async fn an_elapsed_period_does_not_hold_a_cap_reduction_hostage() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 60, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    // The next period has consumed nothing, so last period's total is history.
    h.set_now(datetime!(2026-03-19 00:00:00 UTC));
    h.quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(10)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("a lowered cap governs the period the Quota is now in");
    h.down().await;
}

#[tokio::test]
async fn a_credit_event_names_the_principal_that_authorized_it() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    h.store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 4, &partial("c1", 9), &[])
        .await
        .expect("credit");

    let adjusted = enqueued_messages(&h.db)
        .await
        .into_iter()
        .find(|message| {
            message.payload_type == NotificationEventKind::QuotaCounterAdjusted.as_str()
        })
        .expect("the credit enqueued its event");
    let payload: serde_json::Value =
        serde_json::from_slice(&adjusted.payload).expect("event payload");
    assert_eq!(
        payload["payload"]["principal"],
        serde_json::json!(actor().subject_id),
        "the consumer identity travels with the event, not only to the log"
    );
    h.down().await;
}

#[tokio::test]
async fn a_rollback_event_names_the_principal_that_authorized_it() {
    let h = Harness::up().await;
    h.allocation_quota("u1", Some(100)).await;
    let original = write(OperationType::Debit, "k1", 1);
    h.debit("u1", 30, &original).await.expect("debit");

    h.store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &RollbackTarget {
                original: original.scope.clone(),
                authorized: authorized(),
            },
            &write(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect("rollback");

    let applied = enqueued_messages(&h.db)
        .await
        .into_iter()
        .find(|message| {
            message.payload_type == NotificationEventKind::QuotaRollbackApplied.as_str()
        })
        .expect("the rollback enqueued its event");
    let payload: serde_json::Value =
        serde_json::from_slice(&applied.payload).expect("event payload");
    assert_eq!(
        payload["payload"]["principal"],
        serde_json::json!(actor().subject_id)
    );
    assert_eq!(
        payload["payload"]["original_idempotency_key"],
        serde_json::json!("k1")
    );
    h.down().await;
}

#[tokio::test]
async fn a_debit_that_waits_out_a_boundary_charges_the_period_it_commits_in() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(100), Vec::new()).await;
    h.debit("u1", 10, &write(OperationType::Debit, "k1", 1))
        .await
        .expect("first debit");

    // The clock advances past the boundary between the caller's request and
    // the transaction's locks; the debit belongs to the successor period.
    h.set_now(datetime!(2026-03-18 00:00:00 UTC));
    h.debit("u1", 7, &write(OperationType::Debit, "k2", 2))
        .await
        .expect("debit after the boundary");

    assert_eq!(
        h.consumed("u1", id).await,
        7,
        "the closing period keeps its own total"
    );
    h.down().await;
}

// --- lease accounting (I4, I5) ---------------------------------------------

/// One hour, comfortably inside the platform's TTL window.
const TTL: std::time::Duration = std::time::Duration::from_hours(1);

#[tokio::test]
async fn an_expired_hold_stops_counting_against_capacity_before_any_sweep() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;

    h.acquire("u1", 100, TTL, &write(OperationType::Reserve, "r1", 1))
        .await
        .expect("the whole cap is held");
    assert_eq!(h.consumed("u1", id).await, 100);

    // Past the TTL, with no sweeper anywhere near it.
    h.set_now(DAY_ONE + time::Duration::hours(2));
    assert_eq!(
        h.consumed("u1", id).await,
        0,
        "a read subtracts what an expired hold still occupies (I4)"
    );
    let admitted = h
        .debit("u1", 100, &write(OperationType::Debit, "d1", 2))
        .await
        .expect("debit");
    assert!(
        matches!(
            admitted.get().decision.result,
            quota_enforcement_sdk::DecisionResult::Allowed
        ),
        "an unreclaimed expired hold blocks nothing"
    );
    assert_eq!(h.consumed("u1", id).await, 100, "only the debit counts");
    h.down().await;
}

#[tokio::test]
async fn the_first_writer_returns_an_expired_hold_and_a_later_sweep_moves_nothing() {
    // The sequence a read-side correction alone gets wrong: a credit that
    // floors, a debit after it, and only then the sweeper. Returning the hold
    // twice would erase the debit that arrived in between.
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(1000)).await;

    h.debit("u1", 20, &write(OperationType::Debit, "d1", 1))
        .await
        .expect("debit");
    h.acquire("u1", 80, TTL, &write(OperationType::Reserve, "r1", 2))
        .await
        .expect("acquire");
    assert_eq!(h.consumed("u1", id).await, 100, "20 debited plus 80 held");

    h.set_now(DAY_ONE + time::Duration::hours(2));
    assert_eq!(h.consumed("u1", id).await, 20, "the hold expired");

    h.store
        .apply_credit(&ctx(), &scope_for(tenant()), id, 50, &partial("c1", 3), &[])
        .await
        .expect("credit");
    assert_eq!(h.consumed("u1", id).await, 0, "a credit floors at zero");

    h.debit("u1", 30, &write(OperationType::Debit, "d2", 4))
        .await
        .expect("debit after the credit");
    assert_eq!(h.consumed("u1", id).await, 30);

    let reclaimed = h
        .store
        .reclaim_expired_leases(10, DAY_ONE + time::Duration::hours(2))
        .await
        .expect("sweep");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(
        h.consumed("u1", id).await,
        30,
        "the hold was already returned, so the sweep moves nothing"
    );
    h.down().await;
}

#[tokio::test]
async fn a_period_holding_a_live_lease_is_not_settled() {
    let h = Harness::up().await;
    let id = h.consumption_quota("u1", Some(1000), Vec::new()).await;
    // A TTL that outlives the boundary, which is what opens a settlement
    // window at all. The store takes the TTL as given; bounding it to
    // `[min_lease_ttl, max_lease_ttl]` is the gear's job.
    let across_the_boundary = std::time::Duration::from_hours(48);
    h.acquire(
        "u1",
        10,
        across_the_boundary,
        &write(OperationType::Reserve, "r1", 1),
    )
    .await
    .expect("acquire inside day one");

    // Day two: the debit would normally settle day one and emit its rollover.
    h.set_now(DAY_ONE + time::Duration::days(1));
    h.debit("u1", 5, &write(OperationType::Debit, "d1", 2))
        .await
        .expect("debit in the new period");
    assert!(
        !h.events()
            .await
            .iter()
            .any(|kind| kind == "period-rollover"),
        "a period a live lease can still settle against stays open (I5)"
    );

    // Once the lease has resolved, the next writer closes the period.
    h.set_now(DAY_ONE + time::Duration::days(3));
    h.store
        .reclaim_expired_leases(10, DAY_ONE + time::Duration::days(3))
        .await
        .expect("sweep");
    h.debit("u1", 5, &write(OperationType::Debit, "d2", 3))
        .await
        .expect("debit after the lease resolved");
    assert!(
        h.events()
            .await
            .iter()
            .any(|kind| kind == "period-rollover"),
        "with no live lease left the closing period settles"
    );
    let _ = id;
    h.down().await;
}

#[tokio::test]
async fn a_zero_commit_reverses_as_a_no_op_while_a_denied_debit_stays_irreversible() {
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    let acquired = h
        .acquire("u1", 40, TTL, &write(OperationType::Reserve, "r1", 1))
        .await
        .expect("acquire");
    let token = acquired.get().token.expect("an allowed acquisition holds");

    h.store
        .commit_lease(
            &ctx(),
            &scope_for(tenant()),
            token,
            Some(0),
            &partial("c1", 2),
            &[],
        )
        .await
        .expect("a job that used nothing commits nothing");
    assert_eq!(h.consumed("u1", id).await, 0, "every hold came back");

    // The commit is a real operation, so its rollback succeeds with nothing to
    // move. The subject key is the acquisition's, which the commit reused.
    let commit_scope = IdempotencyScope {
        tenant_id: tenant(),
        subject_key: IdempotencySubjectKey::of(&[user("u1")]),
        operation_type: OperationType::Commit,
        key: "c1".to_owned(),
    };
    h.store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &RollbackTarget {
                original: commit_scope,
                authorized: authorized(),
            },
            &write(OperationType::Rollback, "rb", 3),
            &[],
        )
        .await
        .expect("a zero commit reverses as a no-op");

    // A debit that moved nothing is not a committed debit.
    let denied = h
        .debit("u1", 1_000, &write(OperationType::Debit, "d1", 4))
        .await
        .expect("a denial is a successful call");
    assert!(matches!(
        denied.get().decision.result,
        quota_enforcement_sdk::DecisionResult::Denied { .. }
    ));
    let refused = h
        .store
        .apply_rollback(
            &ctx(),
            &scope_for(tenant()),
            &RollbackTarget {
                original: write(OperationType::Debit, "d1", 4).scope,
                authorized: authorized(),
            },
            &write(OperationType::Rollback, "rb2", 5),
            &[],
        )
        .await
        .expect_err("a denied debit is not reversible");
    assert!(matches!(refused, StorageError::OperationNotFound { .. }));
    h.down().await;
}

#[tokio::test]
async fn an_expired_hold_does_not_hold_a_cap_reduction_hostage() {
    // The cap guard is a writer on the counter row, so it reconciles before it
    // judges: capacity an expired lease no longer holds must not block a
    // reduction until some sweeper happens to run (I4, I6). What it must still
    // see is the usage that is real.
    let h = Harness::up().await;
    let id = h.allocation_quota("u1", Some(100)).await;
    h.debit("u1", 30, &write(OperationType::Debit, "d1", 9))
        .await
        .expect("debit");
    h.acquire("u1", 40, TTL, &write(OperationType::Reserve, "r1", 1))
        .await
        .expect("acquire");
    assert_eq!(h.consumed("u1", id).await, 70, "30 debited plus 40 held");

    let blocked = h
        .quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(50)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("a live hold does block a reduction below it");
    assert!(matches!(
        blocked,
        crate::domain::ports::StoreError::CapBelowConsumed {
            new_cap: 50,
            consumed: 70
        }
    ));

    // The TTL passes; nothing sweeps.
    h.set_now(DAY_ONE + time::Duration::hours(2));

    // This is the call that reconciles, and it must judge against the 30 that
    // is really used — not against the reconciled counter minus the returned
    // amount a second time, which would read zero and let this through. It has
    // to come first: once the hold is stamped returned, a later call has
    // nothing left to subtract twice and the fault hides.
    let below_usage = h
        .quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(20)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("a cap below the real usage is refused");
    assert!(matches!(
        below_usage,
        crate::domain::ports::StoreError::CapBelowConsumed {
            new_cap: 20,
            consumed: 30
        }
    ));

    // And the reduction the expired hold was wrongly blocking now goes through.
    h.quotas
        .update_quota(
            &actor(),
            &scope_for(tenant()),
            id,
            quota_enforcement_sdk::QuotaPatch {
                cap: Some(quota_enforcement_sdk::CapPatch::Bounded(50)),
                ..quota_enforcement_sdk::QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("an expired hold holds nothing hostage");
    assert_eq!(
        h.consumed("u1", id).await,
        30,
        "the hold came back, the debit stayed"
    );
    h.down().await;
}
