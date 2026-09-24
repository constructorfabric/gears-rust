//! The doubles must hold the contract semantics the gear's tests rely on.

use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{
    InMemoryStorage, ScriptedEvaluator, bundle_with_global_policy, empty_engine_config,
    quota_draft, test_metric, test_subject, test_tenant,
};
use crate::engine::{EvaluationContext, EvaluationLimits, PolicySchemaSnapshot};
use crate::models::{
    ApplicableQuotas, AttributionDigest, BatchDebitItem, BootstrapBundle, CapPatch, ConfigDefaults,
    DecisionResult, EventId, IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, LeaseState,
    LeaseToken, NotificationEvent, NotificationEventKind, OperationType, PageRequest, PayloadHash,
    PolicyDraft, PolicyId, PolicyScope, PolicyUpdate, PolicyVersionState, QuotaFilter, QuotaId,
    QuotaPatch, QuotaStatus,
};
use crate::models::{
    EvaluatedDebit, EvaluatedLease, PartialIdempotencyWrite, PeriodType, Retention, RollbackTarget,
    TransitionOutcome,
};
use crate::storage_plugin::{
    CONTRACT_MAJOR, EvaluatedBatch, EvaluatedMutation, QuotaEnforcementStoragePluginV1,
    StorageError,
};

/// The scripted evaluator as the owned callback the contract now takes.
///
/// The double and the real plugin both move it into their transaction, so it
/// cannot be a borrow of a local.
fn scripted(evaluator: &Arc<ScriptedEvaluator>) -> Arc<crate::engine::TransactionEvaluator> {
    let evaluator = Arc::clone(evaluator);
    Arc::new(move |context: &EvaluationContext<'_>| evaluator.evaluate(context))
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(test_tenant().as_uuid())
        .build()
        .expect("test security context")
}

fn scope() -> AccessScope {
    AccessScope::for_tenant(test_tenant().as_uuid())
}

fn idem(op: OperationType, key: &str, payload: u8) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: test_tenant(),
            subject_key: IdempotencySubjectKey::from_bytes([1; 32]),
            operation_type: op,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

/// The caller's half of a settlement's key. Commit and release cannot name a
/// subject key: storage completes the scope from the one the acquisition
/// persisted.
fn partial_idem(key: &str, payload: u8) -> PartialIdempotencyWrite {
    PartialIdempotencyWrite {
        tenant_id: test_tenant(),
        key: key.to_owned(),
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(5).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

/// A double carrying the global policy an evaluated mutation selects inside
/// its own transaction.
async fn storage_with_policy() -> InMemoryStorage {
    let storage = InMemoryStorage::new();
    storage
        .bootstrap(&bundle_with_global_policy())
        .await
        .expect("bootstrap");
    storage
}

/// The two primitives that evaluate inside their own transaction, with the
/// scripted evaluator and an empty environment supplied for the caller.
trait EvaluatedMutations {
    async fn apply_debit_plan_for(
        &self,
        amount: u64,
        write: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError>;

    async fn acquire_lease_for(
        &self,
        amount: u64,
        ttl: Duration,
        write: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError>;
}

impl EvaluatedMutations for InMemoryStorage {
    async fn apply_debit_plan_for(
        &self,
        amount: u64,
        write: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        let storage = self;
        let evaluator = Arc::new(ScriptedEvaluator::new());
        let call = scripted(&evaluator);
        let applicable = applicable();
        storage
            .apply_debit_plan(
                &ctx(),
                &scope(),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &serde_json::Value::Null,
                    resource: &serde_json::Value::Null,
                    user_projection: None,
                    limits: limits(),
                    idempotency: write,
                    authorized: authorized(),
                    evaluate: Arc::clone(&call),
                },
                &[],
            )
            .await
    }

    async fn acquire_lease_for(
        &self,
        amount: u64,
        ttl: Duration,
        write: &IdempotencyWrite,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        let storage = self;
        let evaluator = Arc::new(ScriptedEvaluator::new());
        let call = scripted(&evaluator);
        let applicable = applicable();
        storage
            .acquire_lease(
                &ctx(),
                &scope(),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &serde_json::Value::Null,
                    resource: &serde_json::Value::Null,
                    user_projection: None,
                    limits: limits(),
                    idempotency: write,
                    authorized: authorized(),
                    evaluate: Arc::clone(&call),
                },
                ttl,
            )
            .await
    }
}

/// The token of an acquisition the evaluator allowed.
fn token_of(outcome: &TransitionOutcome<EvaluatedLease>) -> LeaseToken {
    outcome.get().token.expect("an allowed acquisition holds")
}

/// The attribution digest a debit is authorized under. One value throughout,
/// so a rollback in these tests presents the same one the debit recorded.
fn authorized() -> AttributionDigest {
    AttributionDigest::of_canonical(&serde_json::json!({
        "metric": "gts.cf.qe.metric.type.v1~cf.qe.metric.tokens.v1",
        "subjects": [{"kind": "user", "id": "u1"}],
    }))
    .expect("a constant document serializes")
}

fn applicable() -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: test_tenant(),
        subjects: vec![test_subject("u1")],
        metric: test_metric(),
    }
}

async fn seeded_quota(storage: &InMemoryStorage, cap: Option<u64>) -> QuotaId {
    storage
        .create_quota(&ctx(), &scope(), quota_draft(test_subject("u1"), cap), &[])
        .await
        .expect("create quota")
}

// --- storage bootstrap -----------------------------------------------------

#[tokio::test]
async fn storage_bootstrap_is_idempotent_and_seeds_defaults_once() {
    let storage = InMemoryStorage::new();
    let bundle = BootstrapBundle::foundation();
    storage.bootstrap(&bundle).await.expect("first bootstrap");
    let mut second = bundle.clone();
    second.config_defaults.max_active_leases = 5;
    storage.bootstrap(&second).await.expect("second bootstrap");
    assert_eq!(storage.bootstrap_calls(), 2);
    assert_eq!(
        storage.seeded_defaults(),
        Some(ConfigDefaults::default()),
        "existing rows are kept on re-bootstrap"
    );
    assert_eq!(storage.bootstrapped_bundle(), Some(second));
}

#[tokio::test]
async fn storage_bootstrap_rejects_a_schema_major_mismatch() {
    let storage = InMemoryStorage::with_installed_schema_major(CONTRACT_MAJOR + 1);
    let err = storage
        .bootstrap(&BootstrapBundle::foundation())
        .await
        .expect_err("mismatch must fail");
    assert_eq!(
        err,
        StorageError::SchemaVersionMismatch {
            installed: CONTRACT_MAJOR + 1,
            expected: CONTRACT_MAJOR,
        }
    );
    assert!(
        storage.seeded_defaults().is_none(),
        "nothing is seeded on failure"
    );
    assert_eq!(storage.bootstrap_calls(), 1);
}

#[tokio::test]
async fn storage_bootstrap_seeds_the_global_policy_when_the_bundle_carries_one() {
    let storage = InMemoryStorage::new();
    let mut bundle = BootstrapBundle::foundation();
    bundle.global_policy = Some(PolicyDraft {
        schema_snapshot: PolicySchemaSnapshot::default(),
        scope: PolicyScope::Global,
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: empty_engine_config(),
        timeout_ms: None,
        description: None,
        comment: None,
        created_by: "bootstrap".to_owned(),
    });
    storage.bootstrap(&bundle).await.expect("bootstrap");
    let global = storage
        .read_policy(&PolicyScope::Global)
        .await
        .expect("read")
        .expect("seeded");
    assert_eq!(global.version, 1);
    assert_eq!(global.policy_id, PolicyId::global());
    assert_eq!(global.state, PolicyVersionState::Active);
}

#[tokio::test]
async fn storage_injected_failure_blocks_every_call_until_cleared() {
    let storage = InMemoryStorage::new();
    storage.fail_with(StorageError::Unavailable("down".into()));
    assert!(matches!(
        storage.bootstrap(&BootstrapBundle::foundation()).await,
        Err(StorageError::Unavailable(_))
    ));
    assert!(matches!(
        storage
            .lookup_idempotency(&idem(OperationType::Debit, "k", 1).scope)
            .await,
        Err(StorageError::Unavailable(_))
    ));
    storage.clear_failure();
    storage
        .bootstrap(&BootstrapBundle::foundation())
        .await
        .expect("bootstrap after recovery");
}

// --- storage quota and counter semantics -----------------------------------

#[tokio::test]
async fn storage_debit_plan_mutates_counters_once_and_replays_verbatim() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let write = idem(OperationType::Debit, "k1", 7);
    let first = storage
        .apply_debit_plan_for(40, &write)
        .await
        .expect("first debit");
    assert_eq!(first.get().mutation.counters[0].value, 40);
    assert_eq!(storage.consumed(id), 40);

    let replay = storage
        .apply_debit_plan_for(40, &write)
        .await
        .expect("replay");
    assert_eq!(storage.consumed(id), 40, "replay must not mutate");
    assert!(
        matches!(replay, TransitionOutcome::NoOp(_)),
        "a replay is typed as one"
    );
    assert!(
        replay.get().mutation.counters.is_empty(),
        "a replay moved nothing, so it reports no counter movement"
    );
    assert_eq!(
        replay.get().decision,
        first.get().decision,
        "the stored decision is what a replay returns"
    );

    let mismatch = idem(OperationType::Debit, "k1", 8);
    let err = storage
        .apply_debit_plan_for(1, &mismatch)
        .await
        .expect_err("different payload under the same key");
    assert_eq!(err, StorageError::IdempotencyPayloadMismatch);
    assert!(
        storage
            .lookup_idempotency(&write.scope)
            .await
            .expect("lookup")
            .is_some()
    );
}

#[tokio::test]
async fn storage_update_enforces_cap_versus_consumed_and_bumps_the_version() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    storage
        .apply_debit_plan_for(60, &idem(OperationType::Debit, "d", 1))
        .await
        .expect("debit");
    let err = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                cap: Some(CapPatch::Bounded(50)),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("cap below consumed");
    assert_eq!(
        err,
        StorageError::CapBelowConsumed {
            new_cap: 50,
            consumed: 60
        }
    );
    let updated = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                cap: Some(CapPatch::Unbounded),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("raise to unbounded");
    assert_eq!(updated.cap, None);
    assert_eq!(updated.record_version, 2);
    let missing = storage
        .update_quota(
            &ctx(),
            &scope(),
            QuotaId::generate(),
            QuotaPatch::default(),
            &[],
        )
        .await
        .expect_err("unknown quota");
    assert!(matches!(missing, StorageError::QuotaNotFound { .. }));
}

#[tokio::test]
async fn storage_lease_lifecycle_commit_release_expiry_and_deactivation() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let ttl = Duration::from_mins(1);

    let first_lease = storage
        .acquire_lease_for(30, ttl, &idem(OperationType::Reserve, "r1", 1))
        .await
        .expect("acquire");
    let token = token_of(&first_lease);
    assert_eq!(storage.consumed(id), 30);
    assert_eq!(storage.lease_state(token), Some(LeaseState::Active));

    let over = storage
        .commit_lease(
            &ctx(),
            &scope(),
            token,
            Some(31),
            &partial_idem("c0", 1),
            &[],
        )
        .await
        .expect_err("over-commit");
    assert_eq!(
        over,
        StorageError::OverCommitNotAuthorized {
            reserved: 30,
            actual: 31
        }
    );

    storage
        .commit_lease(
            &ctx(),
            &scope(),
            token,
            Some(20),
            &partial_idem("c1", 1),
            &[],
        )
        .await
        .expect("commit less than reserved");
    assert_eq!(storage.consumed(id), 20, "unused reservation is returned");
    assert_eq!(storage.lease_state(token), Some(LeaseState::Committed));
    let again = storage
        .release_lease(&ctx(), &scope(), token, &partial_idem("x", 1), &[])
        .await
        .expect_err("terminal lease");
    assert_eq!(again, StorageError::LeaseNotActive { token });

    let expiring_lease = storage
        .acquire_lease_for(10, ttl, &idem(OperationType::Reserve, "r2", 1))
        .await
        .expect("second lease");
    let token2 = token_of(&expiring_lease);
    storage.expire_leases();
    let expired = storage
        .commit_lease(&ctx(), &scope(), token2, None, &partial_idem("c2", 1), &[])
        .await
        .expect_err("expired leases are released lazily (I4)");
    assert_eq!(expired, StorageError::LeaseNotActive { token: token2 });
    let reclaimed = storage
        .reclaim_expired_leases(10, OffsetDateTime::now_utc())
        .await
        .expect("reclaim");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].token, token2);
    assert_eq!(storage.consumed(id), 20, "auto-release returned the hold");
    assert_eq!(storage.lease_state(token2), Some(LeaseState::AutoReleased));

    let held_lease = storage
        .acquire_lease_for(5, ttl, &idem(OperationType::Reserve, "r3", 1))
        .await
        .expect("third lease");
    let token3 = token_of(&held_lease);
    let outcome = storage
        .deactivate_quota(&ctx(), &scope(), id, &[])
        .await
        .expect("deactivate");
    assert_eq!(outcome.resolved_leases, vec![token3]);
    assert_eq!(
        storage.lease_state(token3),
        Some(LeaseState::ResolvedByDeactivation)
    );
    // A deactivated Quota leaves the applicable set the transaction builds, so
    // the operation is denied by the policy rather than failing on the row.
    let blocked = storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "z", 1))
        .await
        .expect("a debit with nothing applicable still decides");
    assert!(matches!(
        blocked.get().decision.result,
        DecisionResult::Denied { ref reason, .. } if reason == "NO_APPLICABLE_QUOTA"
    ));
    assert_eq!(storage.consumed(id), 20, "a denial moves no counter");
}

#[tokio::test]
async fn storage_active_lease_cap_is_enforced_from_the_seeded_defaults() {
    let storage = InMemoryStorage::new();
    let mut bundle = bundle_with_global_policy();
    bundle.config_defaults.max_active_leases = 1;
    storage.bootstrap(&bundle).await.expect("bootstrap");
    let _quota = seeded_quota(&storage, None).await;
    storage
        .acquire_lease_for(
            1,
            Duration::from_secs(9),
            &idem(OperationType::Reserve, "a", 1),
        )
        .await
        .expect("first");
    let err = storage
        .acquire_lease_for(
            1,
            Duration::from_secs(9),
            &idem(OperationType::Reserve, "b", 1),
        )
        .await
        .expect_err("cap reached");
    assert_eq!(err, StorageError::LeaseInflightLimitExceeded);
}

#[tokio::test]
async fn storage_snapshot_reads_reflect_scope_and_remaining_capacity() {
    let storage = storage_with_policy().await;
    let _quota = seeded_quota(&storage, Some(10)).await;
    storage
        .apply_debit_plan_for(4, &idem(OperationType::Debit, "d", 1))
        .await
        .expect("debit");
    let snaps = storage
        .read_quota_snapshot(&ctx(), &scope(), &applicable())
        .await
        .expect("snapshot");
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].consumed, 4);
    assert_eq!(snaps[0].remaining, Some(6));
    assert!(snaps[0].currently_within_window);
    let other = ApplicableQuotas {
        subjects: vec![test_subject("someone-else")],
        ..applicable()
    };
    assert!(
        storage
            .read_quota_snapshot(&ctx(), &scope(), &other)
            .await
            .expect("snapshot")
            .is_empty(),
        "another subject sees no quota"
    );
}

// --- storage policies ------------------------------------------------------

#[tokio::test]
async fn storage_policy_versions_update_rollback_and_delete() {
    let storage = InMemoryStorage::new();
    let draft = PolicyDraft {
        schema_snapshot: PolicySchemaSnapshot::default(),
        scope: PolicyScope::Global,
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: empty_engine_config(),
        timeout_ms: None,
        description: None,
        comment: Some("seed".to_owned()),
        created_by: "op".to_owned(),
    };
    let v1 = storage
        .create_policy(&ctx(), draft.clone(), &[])
        .await
        .expect("v1");
    assert_eq!(v1.version, 1);
    let dup = storage
        .create_policy(&ctx(), draft, &[])
        .await
        .expect_err("scope taken");
    assert!(matches!(dup, StorageError::PolicyScopeOccupied { .. }));

    let stale = PolicyUpdate {
        schema_snapshot: None,
        if_match_version: 7,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: None,
        created_by: "op".to_owned(),
    };
    let err = storage
        .update_policy(&ctx(), PolicyId::global(), stale, &[])
        .await
        .expect_err("lost update");
    assert_eq!(
        err,
        StorageError::VersionConflict {
            expected: 7,
            actual: 1
        }
    );

    let v2 = storage
        .update_policy(
            &ctx(),
            PolicyId::global(),
            PolicyUpdate {
                schema_snapshot: None,
                if_match_version: 1,
                engine_id: Some("cel".to_owned()),
                engine_config: None,
                timeout_ms: Some(5),
                comment: None,
                created_by: "op".to_owned(),
            },
            &[],
        )
        .await
        .expect("v2");
    assert_eq!(v2.version, 2);
    assert_eq!(v2.engine_id, "cel");
    let listed = storage
        .list_policy_versions(&PolicyId::global(), PageRequest::default())
        .await
        .expect("list");
    assert_eq!(
        listed.items.iter().map(|m| m.state).collect::<Vec<_>>(),
        vec![PolicyVersionState::Superseded, PolicyVersionState::Active]
    );

    let back = storage
        .rollback_policy(&ctx(), PolicyId::global(), 1, None, &[])
        .await
        .expect("rollback");
    assert!(back.is_applied(), "the pointer moved off version 2");
    let back = back.into_inner();
    assert_eq!(back.version, 1);
    assert_eq!(back.state, PolicyVersionState::Active);
    assert_eq!(
        storage
            .read_policy_version(&PolicyId::global(), 2)
            .await
            .expect("read")
            .map(|v| v.state),
        Some(PolicyVersionState::RolledBack)
    );
    let terminal = storage
        .rollback_policy(&ctx(), PolicyId::global(), 2, None, &[])
        .await
        .expect_err("rolled-back versions never re-activate");
    assert!(matches!(
        terminal,
        StorageError::VersionRolledBack { version: 2, .. }
    ));
    let unknown = storage
        .rollback_policy(&ctx(), PolicyId::global(), 9, None, &[])
        .await
        .expect_err("unknown version");
    assert!(matches!(
        unknown,
        StorageError::UnknownPolicyVersion { version: 9, .. }
    ));

    assert_eq!(
        storage
            .delete_policy(&ctx(), PolicyId::global(), None, &[])
            .await,
        Err(StorageError::CannotDeleteSeededGlobalPolicy)
    );
    assert!(
        storage
            .read_policy(&PolicyScope::Global)
            .await
            .expect("read")
            .is_some()
    );
}

#[tokio::test]
async fn storage_reclaims_expired_idempotency_records_and_log_entries() {
    let storage = storage_with_policy().await;
    let _quota = seeded_quota(&storage, None).await;
    storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "d", 1))
        .await
        .expect("debit");
    let far_future = OffsetDateTime::now_utc() + time::Duration::days(10);
    assert_eq!(
        storage
            .reclaim_expired_idempotency(10, OffsetDateTime::now_utc())
            .await
            .expect("none yet"),
        0
    );
    assert_eq!(
        storage
            .reclaim_expired_idempotency(10, far_future)
            .await
            .expect("reclaim"),
        1
    );
    assert_eq!(
        storage
            .reclaim_operation_log(1, far_future)
            .await
            .expect("log"),
        1
    );
    assert_eq!(
        storage
            .reclaim_operation_log(10, far_future)
            .await
            .expect("log again"),
        1
    );
    assert_eq!(
        storage
            .reclaim_operation_log(10, far_future)
            .await
            .expect("empty"),
        0
    );
}

#[tokio::test]
async fn active_projection_bindings_are_the_distinct_pairs_of_active_quotas() {
    let storage = InMemoryStorage::new();
    assert!(
        storage
            .read_active_projection_bindings()
            .await
            .expect("empty store")
            .is_empty()
    );

    let first = storage
        .create_quota(
            &ctx(),
            &scope(),
            quota_draft(test_subject("u1"), Some(10)),
            &[],
        )
        .await
        .expect("first");
    storage
        .create_quota(
            &ctx(),
            &scope(),
            quota_draft(test_subject("u2"), Some(10)),
            &[],
        )
        .await
        .expect("same pair, other subject id");
    let bindings = storage
        .read_active_projection_bindings()
        .await
        .expect("bindings");
    assert_eq!(
        bindings.len(),
        1,
        "distinct by (metric, projection_type): {bindings:?}"
    );
    let binding = bindings.iter().next().expect("one");
    assert_eq!(binding.metric, test_metric());
    assert_eq!(binding.projection_type, test_subject("u1").projection_type);

    storage
        .deactivate_quota(&ctx(), &scope(), first, &[])
        .await
        .expect("deactivate one");
    assert_eq!(
        storage
            .read_active_projection_bindings()
            .await
            .expect("bindings")
            .len(),
        1,
        "the other active Quota keeps the pair"
    );

    storage.fail_with(StorageError::Unavailable("db down".into()));
    assert!(matches!(
        storage.read_active_projection_bindings().await,
        Err(StorageError::Unavailable(_))
    ));
}

// --- quota lifecycle reference semantics -----------------------------------

fn quota_changed(quota_id: Option<QuotaId>) -> NotificationEvent {
    NotificationEvent {
        event_id: EventId::generate(),
        kind: NotificationEventKind::QuotaChanged,
        scope: crate::NotificationScope::Tenant {
            tenant_id: test_tenant(),
        },
        quota_id,
        policy_id: None,
        subject: None,
        payload: serde_json::json!({ "change_kind": "created" }),
        emitted_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
async fn storage_create_fills_the_quota_id_on_events_that_lack_it() {
    let storage = InMemoryStorage::new();
    let id = storage
        .create_quota(
            &ctx(),
            &scope(),
            quota_draft(test_subject("u1"), Some(10)),
            &[quota_changed(None)],
        )
        .await
        .expect("create");
    let events = storage.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].quota_id, Some(id));
}

#[tokio::test]
async fn storage_update_rejects_thresholds_on_an_unbounded_merged_row() {
    let storage = InMemoryStorage::new();
    let mut draft = quota_draft(test_subject("u1"), Some(100));
    draft.notification_thresholds = vec![50];
    let id = storage
        .create_quota(&ctx(), &scope(), draft, &[])
        .await
        .expect("create");

    let unbound_only = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                cap: Some(CapPatch::Unbounded),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("thresholds stay, cap goes: I14");
    assert_eq!(unbound_only, StorageError::ThresholdsRequireBoundedCap);
    assert_eq!(
        storage.quota(id).expect("row").record_version,
        1,
        "a rejected patch writes nothing"
    );

    let unbound_and_clear = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                cap: Some(CapPatch::Unbounded),
                notification_thresholds: Some(Vec::new()),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("both in one patch is consistent");
    assert_eq!(unbound_and_clear.cap, None);
    assert!(unbound_and_clear.notification_thresholds.is_empty());

    let thresholds_on_unbounded = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                notification_thresholds: Some(vec![80]),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("thresholds on an unbounded row: I14");
    assert_eq!(
        thresholds_on_unbounded,
        StorageError::ThresholdsRequireBoundedCap
    );
}

#[tokio::test]
async fn storage_deactivation_is_terminal_returns_held_capacity_and_skips_expired_leases() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let ttl = Duration::from_mins(1);

    let expired_outcome = storage
        .acquire_lease_for(30, ttl, &idem(OperationType::Reserve, "e", 1))
        .await
        .expect("lease that will expire");
    let expired = token_of(&expired_outcome);
    storage.expire_leases();
    let live_outcome = storage
        .acquire_lease_for(5, ttl, &idem(OperationType::Reserve, "l", 1))
        .await
        .expect("live lease");
    let live = token_of(&live_outcome);
    // The second acquisition is a writer on this row, so it gives back the
    // expired hold before adding its own: 30 returned, 5 held (I4).
    assert_eq!(storage.consumed(id), 5);

    let outcome = storage
        .deactivate_quota(&ctx(), &scope(), id, &[])
        .await
        .expect("deactivate");
    assert_eq!(
        outcome.resolved_leases,
        vec![live],
        "expired leases are not resolved (I4)"
    );
    assert_eq!(storage.lease_state(expired), Some(LeaseState::Active));
    assert_eq!(storage.consumed(id), 0, "the live hold was returned");
    let row = storage.quota(id).expect("row");
    assert_eq!(row.status, QuotaStatus::Deactivated);
    assert_eq!(row.record_version, 2);

    let again = storage
        .deactivate_quota(&ctx(), &scope(), id, &[])
        .await
        .expect_err("no second cascade");
    assert_eq!(again, StorageError::QuotaDeactivated { id });
    assert_eq!(storage.quota(id).expect("row").record_version, 2);

    let patched = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                fail_open_hint: Some(true),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("a deactivated Quota accepts no patch");
    assert_eq!(patched, StorageError::QuotaDeactivated { id });
}

#[tokio::test]
async fn storage_active_quota_counts_cover_active_quotas_only() {
    let storage = InMemoryStorage::new();
    let zero = seeded_quota(&storage, Some(0)).await;
    seeded_quota(&storage, None).await;
    seeded_quota(&storage, Some(10)).await;
    let counts = storage.read_active_quota_counts().await.expect("counts");
    assert_eq!((counts.cap_zero, counts.cap_unbounded), (1, 1));
    assert_eq!(counts.by_metric.get(&test_metric()), Some(&3));

    storage
        .deactivate_quota(&ctx(), &scope(), zero, &[])
        .await
        .expect("deactivate");
    let counts = storage.read_active_quota_counts().await.expect("counts");
    assert_eq!((counts.cap_zero, counts.cap_unbounded), (0, 1));
    assert_eq!(counts.by_metric.get(&test_metric()), Some(&2));

    storage.fail_with(StorageError::Unavailable("db down".into()));
    assert!(matches!(
        storage.read_active_quota_counts().await,
        Err(StorageError::Unavailable(_))
    ));
}

#[tokio::test]
async fn storage_read_quotas_filters_and_pages_in_creation_order() {
    let storage = InMemoryStorage::new();
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(
            storage
                .create_quota(
                    &ctx(),
                    &scope(),
                    quota_draft(test_subject(&format!("u{n}")), Some(n)),
                    &[],
                )
                .await
                .expect("create"),
        );
    }
    storage
        .deactivate_quota(&ctx(), &scope(), ids[4], &[])
        .await
        .expect("deactivate the last one");

    let first = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter::default(),
            PageRequest::first(2),
        )
        .await
        .expect("page 1");
    assert_eq!(
        first.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        ids[..2]
    );
    let second = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter::default(),
            PageRequest {
                limit: 2,
                cursor: first.next_cursor.clone(),
            },
        )
        .await
        .expect("page 2");
    assert_eq!(
        second.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        ids[2..4]
    );
    let third = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter::default(),
            PageRequest {
                limit: 2,
                cursor: second.next_cursor.clone(),
            },
        )
        .await
        .expect("page 3");
    assert_eq!(
        third.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        ids[4..]
    );
    assert!(third.next_cursor.is_none());

    let active = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter {
                status: Some(QuotaStatus::Active),
                ..QuotaFilter::default()
            },
            PageRequest::default(),
        )
        .await
        .expect("active only");
    assert_eq!(active.items.len(), 4);
    let by_subject = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter {
                subject: Some(test_subject("u3")),
                ..QuotaFilter::default()
            },
            PageRequest::default(),
        )
        .await
        .expect("by subject");
    assert_eq!(
        by_subject.items.iter().map(|q| q.id).collect::<Vec<_>>(),
        vec![ids[3]]
    );
    let by_ids = storage
        .read_quotas(
            &ctx(),
            &scope(),
            QuotaFilter {
                ids: vec![ids[0], ids[4]],
                ..QuotaFilter::default()
            },
            PageRequest::default(),
        )
        .await
        .expect("by ids");
    assert_eq!(by_ids.items.len(), 2, "deactivated rows stay readable");
}

#[tokio::test]
async fn storage_read_quotas_refuses_a_cursor_it_did_not_issue() {
    let storage = InMemoryStorage::new();
    storage
        .create_quota(
            &ctx(),
            &scope(),
            quota_draft(test_subject("u"), Some(1)),
            &[],
        )
        .await
        .expect("created");
    for cursor in ["not-a-cursor", "", "-1", "1.5"] {
        let err = storage
            .read_quotas(
                &ctx(),
                &scope(),
                QuotaFilter::default(),
                PageRequest {
                    limit: 10,
                    cursor: Some(cursor.to_owned()),
                },
            )
            .await
            .expect_err(cursor);
        assert_eq!(err, StorageError::InvalidCursor, "{cursor:?}");
    }
}

#[tokio::test]
async fn storage_update_quota_moves_the_contract_reference_with_the_metadata() {
    let storage = InMemoryStorage::new();
    let id = storage
        .create_quota(
            &ctx(),
            &scope(),
            quota_draft(test_subject("u"), Some(10)),
            &[],
        )
        .await
        .expect("created");
    let before = storage.quota(id).expect("stored").constraint_contract;
    let v2 = crate::models::ContractRef {
        type_id: gts::GtsTypeId::new(
            "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v2~",
        ),
        version: 2,
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert("weight".to_owned(), serde_json::json!(3));

    let err = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                metadata: Some(metadata.clone()),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect_err("metadata without its contract");
    assert!(matches!(err, StorageError::Internal(_)), "{err:?}");
    assert_eq!(storage.quota(id).expect("stored").record_version, 1);

    let updated = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                metadata: Some(metadata),
                constraint_contract: Some(v2.clone()),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("metadata with its contract");
    assert_ne!(before, v2);
    assert_eq!(updated.constraint_contract, v2);
    assert_eq!(updated.metadata["weight"], serde_json::json!(3));
    let unrelated = storage
        .update_quota(
            &ctx(),
            &scope(),
            id,
            QuotaPatch {
                fail_open_hint: Some(true),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("non-metadata patch");
    assert_eq!(
        unrelated.constraint_contract, v2,
        "other patches leave it alone"
    );
}

#[tokio::test]
async fn deleted_policy_history_survives_scope_recreation_and_replays_are_noops() {
    let storage = InMemoryStorage::new();
    let draft = PolicyDraft {
        schema_snapshot: PolicySchemaSnapshot::default(),
        scope: PolicyScope::Metric {
            metric: test_metric(),
        },
        engine_id: "most-restrictive-wins".into(),
        engine_config: empty_engine_config(),
        timeout_ms: None,
        description: None,
        comment: Some("original".into()),
        created_by: "operator".into(),
    };
    let first = storage
        .create_policy(&ctx(), draft.clone(), &[])
        .await
        .expect("create");
    assert!(
        storage
            .delete_policy(&ctx(), first.policy_id.clone(), Some("retire".into()), &[])
            .await
            .expect("delete")
            .is_applied()
    );
    let count = storage.policy_audit().len();
    assert!(
        !storage
            .delete_policy(&ctx(), first.policy_id.clone(), None, &[])
            .await
            .expect("replay")
            .is_applied(),
        "a repeated delete reports a no-op"
    );
    assert_eq!(storage.policy_audit().len(), count);
    assert_eq!(
        storage
            .policy_audit()
            .last()
            .and_then(|entry| entry.comment.as_deref()),
        Some("retire")
    );
    let second = storage
        .create_policy(&ctx(), draft, &[])
        .await
        .expect("recreate");
    assert_ne!(first.policy_id, second.policy_id);
    assert_eq!(second.version, 1);
    let old = storage
        .read_policy_version(&first.policy_id, 1)
        .await
        .expect("read")
        .expect("retained");
    assert_eq!(old.state, PolicyVersionState::Deleted);
    assert_eq!(old.comment.as_deref(), Some("original"));
    assert!(matches!(
        storage
            .rollback_policy(&ctx(), first.policy_id, 1, None, &[])
            .await,
        Err(StorageError::PolicyDeleted { .. })
    ));
    assert!(matches!(
        storage
            .delete_policy(&ctx(), PolicyId::new("never-created"), None, &[])
            .await,
        Err(StorageError::PolicyNotFound { .. })
    ));
}

#[tokio::test]
async fn rollback_preserves_comments_and_update_allocates_above_high_water() {
    let storage = InMemoryStorage::new();
    let first = storage
        .create_policy(
            &ctx(),
            PolicyDraft {
                schema_snapshot: PolicySchemaSnapshot::default(),
                scope: PolicyScope::Global,
                engine_id: "most-restrictive-wins".into(),
                engine_config: empty_engine_config(),
                timeout_ms: None,
                description: None,
                comment: Some("created".into()),
                created_by: "operator".into(),
            },
            &[],
        )
        .await
        .expect("create");
    let patch = |version| PolicyUpdate {
        schema_snapshot: None,
        if_match_version: version,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: None,
        created_by: "operator".into(),
    };
    storage
        .update_policy(&ctx(), first.policy_id.clone(), patch(1), &[])
        .await
        .expect("v2");
    storage
        .update_policy(&ctx(), first.policy_id.clone(), patch(2), &[])
        .await
        .expect("v3");
    let back = storage
        .rollback_policy(
            &ctx(),
            first.policy_id.clone(),
            1,
            Some("rollback note".into()),
            &[],
        )
        .await
        .expect("rollback");
    assert!(back.is_applied());
    assert_eq!(back.into_inner().comment.as_deref(), Some("created"));
    let audit = storage.policy_audit();
    assert_eq!(
        audit.last().and_then(|entry| entry.comment.as_deref()),
        Some("rollback note")
    );
    assert!(
        !storage
            .rollback_policy(
                &ctx(),
                first.policy_id.clone(),
                1,
                Some("retry".into()),
                &[],
            )
            .await
            .expect("replay")
            .is_applied(),
        "rolling back onto the already-active target reports a no-op"
    );
    assert_eq!(storage.policy_audit(), audit);
    assert_eq!(
        storage
            .update_policy(&ctx(), first.policy_id, patch(1), &[])
            .await
            .expect("v4")
            .version,
        4
    );
}

// --- the in-transaction evaluation convention ------------------------------

#[tokio::test]
async fn an_evaluated_debit_records_what_the_transaction_decided_not_what_a_caller_claimed() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(10)).await;
    let write = idem(OperationType::Debit, "over", 3);

    // The caller states an amount; the plan and the verdict are the
    // transaction's, computed against rows it holds.
    let denied = storage
        .apply_debit_plan_for(40, &write)
        .await
        .expect("a refusal is a decision, not a failure");
    assert!(matches!(
        denied.get().decision.result,
        DecisionResult::Denied { ref reason, .. } if reason == "QUOTA_EXCEEDED"
    ));
    assert_eq!(storage.consumed(id), 0, "a denial moves no counter");

    // The record is attributed to the policy the transaction selected.
    let record = storage
        .lookup_idempotency(&write.scope)
        .await
        .expect("lookup")
        .expect("a denial still occupies the key");
    assert_eq!(record.engine_id.as_deref(), Some(super::TEST_ENGINE_ID));
    assert_eq!(record.policy_id, Some(PolicyId::global()));
    assert_eq!(record.policy_version, Some(1));

    // A replay of a denial denies again, without a second evaluation.
    let replay = storage
        .apply_debit_plan_for(40, &write)
        .await
        .expect("replay");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert!(matches!(
        replay.get().decision.result,
        DecisionResult::Denied { .. }
    ));
}

#[tokio::test]
async fn a_transaction_that_needs_a_prepared_artifact_rolls_back_and_writes_nothing() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let write = idem(OperationType::Debit, "miss", 4);
    let evaluator = Arc::new(ScriptedEvaluator::with_preparation_misses(1));
    let call = scripted(&evaluator);
    let applicable = applicable();
    let err = storage
        .apply_debit_plan(
            &ctx(),
            &scope(),
            &EvaluatedMutation {
                applicable: &applicable,
                amount: 5,
                request: &serde_json::Value::Null,
                resource: &serde_json::Value::Null,
                user_projection: None,
                limits: limits(),
                idempotency: &write,
                authorized: authorized(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect_err("the artifact is not resident");
    assert_eq!(
        err,
        StorageError::PreparationRequired {
            policy_id: PolicyId::global(),
            version: 1,
        }
    );
    assert_eq!(storage.consumed(id), 0, "nothing was applied");
    assert!(
        storage
            .lookup_idempotency(&write.scope)
            .await
            .expect("lookup")
            .is_none(),
        "the key stays free, so the retry is not a replay"
    );
    assert_eq!(evaluator.calls(), 1);
}

#[tokio::test]
async fn an_atomic_batch_evaluates_each_item_and_replays_as_one_envelope() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let envelope = idem(OperationType::Debit, "batch", 5);
    let evaluator = Arc::new(ScriptedEvaluator::new());
    let call = scripted(&evaluator);
    let items = vec![
        BatchDebitItem {
            applicable: applicable(),
            amount: 7,
            request: serde_json::Value::Null,
            resource: serde_json::Value::Null,
            authorized: authorized(),
            item_scope: None,
        },
        BatchDebitItem {
            applicable: applicable(),
            amount: 3,
            request: serde_json::Value::Null,
            resource: serde_json::Value::Null,
            authorized: authorized(),
            item_scope: None,
        },
    ];
    let batch = EvaluatedBatch {
        envelope: &envelope,
        items: &items,
        user_projection: None,
        limits: limits(),
        evaluate: Arc::clone(&call),
    };
    let applied = storage
        .apply_batch_debit(&ctx(), &scope(), &batch, &[])
        .await
        .expect("batch");
    assert_eq!(applied.get().len(), 2);
    assert_eq!(storage.consumed(id), 10, "both items applied");
    assert_eq!(evaluator.calls(), 2, "each item is evaluated on its own");

    let replay = storage
        .apply_batch_debit(&ctx(), &scope(), &batch, &[])
        .await
        .expect("replay");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert_eq!(replay.get().len(), 2);
    assert_eq!(
        storage.consumed(id),
        10,
        "a replayed envelope mutates nothing"
    );
    assert_eq!(evaluator.calls(), 2, "and evaluates nothing");
}

#[tokio::test]
async fn selection_prefers_the_metric_policy_and_falls_through_to_global_once_it_is_deleted() {
    let storage = storage_with_policy().await;
    let _quota = seeded_quota(&storage, Some(100)).await;
    let metric_policy = storage
        .create_policy(
            &ctx(),
            PolicyDraft {
                schema_snapshot: PolicySchemaSnapshot::default(),
                scope: PolicyScope::Metric {
                    metric: test_metric(),
                },
                engine_id: "metric-engine".to_owned(),
                engine_config: empty_engine_config(),
                timeout_ms: None,
                description: None,
                comment: None,
                created_by: "operator".to_owned(),
            },
            &[],
        )
        .await
        .expect("metric policy");

    // The more specific scope wins while it is active.
    let first = storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "s1", 9))
        .await
        .expect("debit");
    assert_eq!(
        first.get().decision.diagnostics["engine_id"],
        "metric-engine"
    );

    // Deleting it falls the metric through to the global policy, inside the
    // same transaction that mutates the counters.
    storage
        .delete_policy(&ctx(), metric_policy.policy_id, None, &[])
        .await
        .expect("delete");
    let after = storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "s2", 9))
        .await
        .expect("debit");
    assert_eq!(
        after.get().decision.diagnostics["engine_id"],
        super::TEST_ENGINE_ID
    );
}

fn batch_item(amount: u64) -> BatchDebitItem {
    BatchDebitItem {
        applicable: applicable(),
        amount,
        request: serde_json::Value::Null,
        resource: serde_json::Value::Null,
        authorized: authorized(),
        item_scope: None,
    }
}

#[tokio::test]
async fn an_atomic_batch_evaluates_against_running_state_and_denies_as_a_whole() {
    // The PRD's worked scenario: one Quota of 800, two items of 500. The first
    // is affordable alone, the pair is not, and nothing may be written.
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(800)).await;
    let envelope = idem(OperationType::Debit, "atomic", 6);
    let evaluator = Arc::new(ScriptedEvaluator::new());
    let call = scripted(&evaluator);
    let items = vec![batch_item(500), batch_item(500)];
    let outcome = storage
        .apply_batch_debit(
            &ctx(),
            &scope(),
            &EvaluatedBatch {
                envelope: &envelope,
                items: &items,
                user_projection: None,
                limits: limits(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect("a denied batch is a decision, not a failure");
    let decided = outcome.get();
    assert!(
        matches!(decided[0].decision.result, DecisionResult::Allowed),
        "the first item is affordable on its own"
    );
    assert!(
        matches!(
            decided[1].decision.result,
            DecisionResult::Denied { ref reason, .. } if reason == "QUOTA_EXCEEDED"
        ),
        "the second sees what the first consumed: {:?}",
        decided[1].decision.result
    );
    assert_eq!(
        storage.consumed(id),
        0,
        "one denial rolls the whole envelope back"
    );
    assert!(
        storage.events().is_empty(),
        "nothing was applied, so nothing is announced"
    );

    // A batch every item can afford commits the union of the plans, each item
    // still evaluated against what its predecessors took.
    let fits = vec![batch_item(500), batch_item(200)];
    let committed = storage
        .apply_batch_debit(
            &ctx(),
            &scope(),
            &EvaluatedBatch {
                envelope: &idem(OperationType::Debit, "fits", 7),
                items: &fits,
                user_projection: None,
                limits: limits(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect("batch");
    assert!(
        committed
            .get()
            .iter()
            .all(|item| matches!(item.decision.result, DecisionResult::Allowed))
    );
    assert_eq!(storage.consumed(id), 700);
}

#[tokio::test]
async fn a_replayed_acquisition_returns_its_own_token_not_another_lease_of_the_subject() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let ttl = Duration::from_mins(1);
    let small = idem(OperationType::Reserve, "small", 1);
    let large = idem(OperationType::Reserve, "large", 2);

    let small_lease = storage
        .acquire_lease_for(10, ttl, &small)
        .await
        .expect("first acquisition");
    let large_lease = storage
        .acquire_lease_for(20, ttl, &large)
        .await
        .expect("second acquisition for the same subject");
    let small_token = token_of(&small_lease);
    let large_token = token_of(&large_lease);
    assert_ne!(small_token, large_token);

    let replayed = storage
        .acquire_lease_for(10, ttl, &small)
        .await
        .expect("replay");
    assert!(matches!(replayed, TransitionOutcome::NoOp(_)));
    assert_eq!(
        token_of(&replayed),
        small_token,
        "a replay returns the lease that acquisition took"
    );
    assert_eq!(storage.consumed(id), 30, "neither replay held again");

    // A denial holds nothing, and its replay must not adopt a live lease.
    let refused = idem(OperationType::Reserve, "refused", 3);
    let denied = storage
        .acquire_lease_for(500, ttl, &refused)
        .await
        .expect("a refusal is a decision");
    assert!(denied.get().token.is_none());
    let denied_replay = storage
        .acquire_lease_for(500, ttl, &refused)
        .await
        .expect("replayed refusal");
    assert!(
        denied_replay.get().token.is_none(),
        "a replayed denial holds nothing"
    );
}

#[tokio::test]
async fn the_budget_is_resolved_against_the_policy_the_transaction_selected() {
    let storage = InMemoryStorage::new();
    let mut bundle = bundle_with_global_policy();
    // The seeded version asks for 3ms, well inside the operator clamp of 5ms.
    if let Some(policy) = bundle.global_policy.as_mut() {
        policy.timeout_ms = Some(3);
    }
    storage.bootstrap(&bundle).await.expect("bootstrap");
    let _quota = seeded_quota(&storage, Some(100)).await;
    let evaluator = Arc::new(ScriptedEvaluator::new());
    let call = scripted(&evaluator);
    let write = idem(OperationType::Debit, "budget", 8);
    let applicable = applicable();
    storage
        .apply_debit_plan(
            &ctx(),
            &scope(),
            &EvaluatedMutation {
                applicable: &applicable,
                amount: 1,
                request: &serde_json::Value::Null,
                resource: &serde_json::Value::Null,
                user_projection: None,
                limits: limits(),
                idempotency: &write,
                authorized: authorized(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect("debit");
    assert_eq!(
        evaluator.observed_timeouts(),
        vec![Duration::from_millis(3)],
        "the selected version's requested timeout reaches the engine, \
         without the caller having resolved it"
    );

    // Raising the version's request past the clamp clamps it, per evaluation
    // and with no artifact republished.
    let active = storage
        .read_policy(&PolicyScope::Global)
        .await
        .expect("read")
        .expect("seeded");
    storage
        .update_policy(
            &ctx(),
            active.policy_id,
            PolicyUpdate {
                schema_snapshot: None,
                if_match_version: active.version,
                engine_id: None,
                engine_config: None,
                timeout_ms: Some(9),
                comment: None,
                created_by: "operator".to_owned(),
            },
            &[],
        )
        .await
        .expect("raise the requested timeout");
    let clamped = idem(OperationType::Debit, "clamped", 9);
    storage
        .apply_debit_plan(
            &ctx(),
            &scope(),
            &EvaluatedMutation {
                applicable: &applicable,
                amount: 1,
                request: &serde_json::Value::Null,
                resource: &serde_json::Value::Null,
                user_projection: None,
                limits: limits(),
                idempotency: &clamped,
                authorized: authorized(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect("debit");
    assert_eq!(
        evaluator.observed_timeouts(),
        vec![Duration::from_millis(3), Duration::from_millis(5)],
        "the new version's request is re-clamped per evaluation, with no \
         artifact republished"
    );
}

// ---------------------------------------------------------------------------
// Consumption periods, thresholds, credit and rollback
// ---------------------------------------------------------------------------

/// 2026-03-17T10:00:00Z, a Tuesday inside an ordinary day period.
fn day_one() -> OffsetDateTime {
    OffsetDateTime::parse(
        "2026-03-17T10:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("valid timestamp")
}

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .expect("valid timestamp")
}

async fn consumption_storage(cap: Option<u64>, thresholds: Vec<u8>) -> (InMemoryStorage, QuotaId) {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = storage
        .create_quota(
            &ctx(),
            &scope(),
            super::consumption_quota_draft(test_subject("u1"), cap, PeriodType::Day, thresholds),
            &[],
        )
        .await
        .expect("create consumption quota");
    (storage, id)
}

fn partial(key: &str, payload: u8) -> PartialIdempotencyWrite {
    PartialIdempotencyWrite {
        tenant_id: test_tenant(),
        key: key.to_owned(),
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

fn target(original: &IdempotencyWrite) -> RollbackTarget {
    RollbackTarget {
        original: original.scope.clone(),
        authorized: authorized(),
    }
}

fn kinds(storage: &InMemoryStorage) -> Vec<NotificationEventKind> {
    storage.events().into_iter().map(|e| e.kind).collect()
}

#[tokio::test]
async fn a_first_debit_materializes_the_current_period_with_no_threshold_marker() {
    let (storage, id) = consumption_storage(Some(100), vec![50, 80]).await;

    storage
        .apply_debit_plan_for(10, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    let rows = storage.period_rows(id);
    assert_eq!(rows.len(), 1, "exactly one period row was materialized");
    assert_eq!(rows[0].window.start, at("2026-03-17T00:00:00Z"));
    assert_eq!(rows[0].consumed, 10);
    assert_eq!(rows[0].marker, None, "no threshold was crossed (I13)");
    assert!(!rows[0].settled);
    assert!(!kinds(&storage).contains(&NotificationEventKind::PeriodRollover));
}

#[tokio::test]
async fn a_debit_at_exactly_the_boundary_opens_the_successor_and_settles_the_closing_row() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    storage
        .apply_debit_plan_for(10, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("first debit");

    // Exactly at period_end: half-open windows put this in the next period.
    storage.set_now(Some(at("2026-03-18T00:00:00Z")));
    storage
        .apply_debit_plan_for(4, &idem(OperationType::Debit, "k2", 2))
        .await
        .expect("debit after the boundary");

    let rows = storage.period_rows(id);
    assert_eq!(rows.len(), 2, "the successor row was materialized");
    assert!(rows[0].settled, "the closing row settled");
    assert_eq!(rows[0].consumed, 10, "the closing counter is final");
    assert_eq!(rows[1].consumed, 4, "the new period starts from this debit");
    assert_eq!(
        kinds(&storage)
            .iter()
            .filter(|kind| **kind == NotificationEventKind::PeriodRollover)
            .count(),
        1,
        "exactly one rollover event per closing row"
    );
}

#[tokio::test]
async fn thresholds_fire_upward_once_and_reset_when_the_period_rolls_over() {
    let (storage, id) = consumption_storage(Some(100), vec![50, 80, 100]).await;

    storage
        .apply_debit_plan_for(85, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("debit past two thresholds");
    let crossed: Vec<NotificationEvent> = storage
        .events()
        .into_iter()
        .filter(|e| e.kind == NotificationEventKind::ThresholdCrossed)
        .collect();
    assert_eq!(
        crossed.len(),
        1,
        "one event carries every threshold crossed"
    );
    assert_eq!(
        crossed[0].payload["crossed_thresholds"],
        serde_json::json!([50, 80])
    );
    assert_eq!(storage.period_rows(id)[0].marker, Some(80));

    // A credit below 80 does not re-arm it.
    storage
        .apply_credit(&ctx(), &scope(), id, 40, &partial("c1", 9), &[])
        .await
        .expect("credit");
    storage
        .apply_debit_plan_for(40, &idem(OperationType::Debit, "k2", 2))
        .await
        .expect("debit back across 80");
    assert_eq!(
        storage
            .events()
            .iter()
            .filter(|e| e.kind == NotificationEventKind::ThresholdCrossed)
            .count(),
        1,
        "a threshold crossed once stays crossed for the period"
    );

    // The next period starts with a clean marker.
    storage.set_now(Some(at("2026-03-18T00:00:00Z")));
    storage
        .apply_debit_plan_for(60, &idem(OperationType::Debit, "k3", 3))
        .await
        .expect("debit in the new period");
    let rows = storage.period_rows(id);
    assert_eq!(rows[1].marker, Some(50), "the successor row armed afresh");
}

#[tokio::test]
async fn a_no_applicable_quota_denial_records_nothing_and_is_reevaluated_later() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let write = idem(OperationType::Debit, "k1", 1);

    let denied = storage
        .apply_debit_plan_for(5, &write)
        .await
        .expect("a denial is a successful call");
    assert_eq!(
        denied.get().decision.denied_reason(),
        Some(crate::models::NO_APPLICABLE_QUOTA)
    );
    assert_eq!(
        denied.get().retention,
        Retention::Unrecorded,
        "nothing was persisted, so nothing may replay or be cached"
    );
    assert!(
        storage
            .lookup_idempotency(&write.scope)
            .await
            .expect("lookup")
            .is_none()
    );

    // Provisioning a Quota must change the answer for the very same request.
    let id = seeded_quota(&storage, Some(100)).await;
    let allowed = storage
        .apply_debit_plan_for(5, &write)
        .await
        .expect("re-evaluated");
    assert_eq!(allowed.get().decision.result, DecisionResult::Allowed);
    assert_eq!(storage.consumed(id), 5);
}

#[tokio::test]
async fn a_credit_derives_its_scope_from_the_locked_row_and_floors_at_zero() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    storage
        .apply_debit_plan_for(10, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");

    let applied = storage
        .apply_credit(&ctx(), &scope(), id, 40, &partial("c1", 9), &[])
        .await
        .expect("credit");
    assert!(matches!(applied, TransitionOutcome::Applied(_)));
    assert_eq!(storage.consumed(id), 0, "a credit floors at zero");
    assert!(kinds(&storage).contains(&NotificationEventKind::QuotaCounterAdjusted));

    // The scope the plugin derived is the Quota's own subject pair.
    let derived = IdempotencySubjectKey::of(&[test_subject("u1")]);
    let record = storage
        .lookup_idempotency(&IdempotencyScope {
            tenant_id: test_tenant(),
            subject_key: derived,
            operation_type: OperationType::Credit,
            key: "c1".to_owned(),
        })
        .await
        .expect("lookup")
        .expect("the credit recorded under the derived scope");
    assert_eq!(record.attribution_hash, None, "a credit reverses nothing");
}

#[tokio::test]
async fn a_credit_replays_even_after_its_quota_was_deactivated() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    storage
        .apply_debit_plan_for(10, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    storage
        .apply_credit(&ctx(), &scope(), id, 4, &partial("c1", 9), &[])
        .await
        .expect("credit");
    storage
        .deactivate_quota(&ctx(), &scope(), id, &[])
        .await
        .expect("deactivate");

    let replay = storage
        .apply_credit(&ctx(), &scope(), id, 4, &partial("c1", 9), &[])
        .await
        .expect("a replay answers from the record, not from the guards");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert_eq!(storage.consumed(id), 6, "the replay moved nothing");

    let fresh = storage
        .apply_credit(&ctx(), &scope(), id, 1, &partial("c2", 8), &[])
        .await
        .expect_err("a fresh credit is refused");
    assert_eq!(fresh, StorageError::QuotaDeactivated { id });
}

#[tokio::test]
async fn a_credit_refuses_a_closed_period_but_materializes_one_that_was_never_opened() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;

    // Never evaluated: the current window is open, so the row is materialized.
    storage
        .apply_credit(&ctx(), &scope(), id, 5, &partial("c1", 9), &[])
        .await
        .expect("a Quota with no row has an open current window");
    assert_eq!(storage.period_rows(id).len(), 1);

    storage.set_now(Some(at("2026-03-19T00:00:00Z")));
    let err = storage
        .apply_credit(&ctx(), &scope(), id, 5, &partial("c2", 8), &[])
        .await
        .expect_err("the latest period has ended");
    assert_eq!(err, StorageError::PeriodClosed);
}

#[tokio::test]
async fn a_rollback_reverses_against_the_acquisition_period_after_the_boundary() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");

    // Cross into the next period, then reverse: the closing row is the one
    // that must lose the amount (I5), not the row now current.
    storage.set_now(Some(at("2026-03-18T00:00:00Z")));
    storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &target(&original),
            &idem(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect("rollback into the settlement window");

    let rows = storage.period_rows(id);
    assert_eq!(rows[0].consumed, 0, "the acquisition period was reversed");
    assert!(kinds(&storage).contains(&NotificationEventKind::QuotaRollbackApplied));
}

#[tokio::test]
async fn a_rollback_is_refused_once_the_attribution_period_has_settled() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");

    // A debit in the next period settles the closing row and emits its event.
    storage.set_now(Some(at("2026-03-18T00:00:00Z")));
    storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "k2", 2))
        .await
        .expect("debit in the new period");

    let err = storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &target(&original),
            &idem(OperationType::Rollback, "r1", 3),
            &[],
        )
        .await
        .expect_err("a settled period is final");
    assert_eq!(err, StorageError::PeriodClosed);
    assert_eq!(storage.period_rows(id)[0].consumed, 30, "nothing moved");
}

#[tokio::test]
async fn a_rollback_authorized_under_another_attribution_finds_nothing() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = seeded_quota(&storage, Some(100)).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");

    let foreign = RollbackTarget {
        original: original.scope.clone(),
        authorized: AttributionDigest::from_bytes([9; 32]),
    };
    let err = storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &foreign,
            &idem(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect_err("the caller was admitted for something else");

    assert_eq!(
        err,
        StorageError::OperationNotFound {
            key: "k1".to_owned()
        },
        "the record is reported absent rather than revealed"
    );
    assert_eq!(storage.consumed(id), 30, "no counter was touched");
}

#[tokio::test]
async fn a_rollback_of_a_credit_a_denial_or_an_unknown_key_finds_nothing() {
    let (storage, id) = consumption_storage(Some(10), Vec::new()).await;
    let credit_key = partial("c1", 9);
    storage
        .apply_credit(&ctx(), &scope(), id, 1, &credit_key, &[])
        .await
        .expect("credit");
    let denied = idem(OperationType::Debit, "denied", 4);
    storage
        .apply_debit_plan_for(999, &denied)
        .await
        .expect("a denial is a successful call");

    for (label, scope_of) in [
        (
            "an unknown key",
            idem(OperationType::Debit, "nope", 1).scope,
        ),
        ("a denied debit", denied.scope.clone()),
        (
            "a credit",
            IdempotencyScope {
                tenant_id: test_tenant(),
                subject_key: IdempotencySubjectKey::of(&[test_subject("u1")]),
                operation_type: OperationType::Credit,
                key: "c1".to_owned(),
            },
        ),
    ] {
        let err = storage
            .apply_rollback(
                &ctx(),
                &scope(),
                &RollbackTarget {
                    original: scope_of,
                    authorized: authorized(),
                },
                &idem(OperationType::Rollback, label, 2),
                &[],
            )
            .await
            .expect_err("only a committed debit is reversible");
        assert!(
            matches!(err, StorageError::OperationNotFound { .. }),
            "{label} should not be reversible, got {err:?}"
        );
    }
}

#[tokio::test]
async fn the_same_original_key_under_another_subject_set_is_a_different_operation() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    seeded_quota(&storage, Some(100)).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");

    let other_subject = IdempotencyScope {
        subject_key: IdempotencySubjectKey::from_bytes([2; 32]),
        ..original.scope.clone()
    };
    let err = storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &RollbackTarget {
                original: other_subject,
                authorized: authorized(),
            },
            &idem(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect_err("the scope includes the subject set");

    assert!(matches!(err, StorageError::OperationNotFound { .. }));
}

#[tokio::test]
async fn a_second_rollback_under_another_key_is_an_idempotent_no_op() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = seeded_quota(&storage, Some(100)).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");
    storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &target(&original),
            &idem(OperationType::Rollback, "r1", 2),
            &[],
        )
        .await
        .expect("first rollback");
    assert_eq!(storage.consumed(id), 0);

    let again = storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &target(&original),
            &idem(OperationType::Rollback, "r2", 3),
            &[],
        )
        .await
        .expect("a second rollback is accepted");

    assert!(
        again.get().decision.debit_plan.is_empty(),
        "reversal happens once, so the second plan is empty"
    );
    assert_eq!(storage.consumed(id), 0, "the counter did not go negative");
}

#[tokio::test]
async fn a_rollback_replay_returns_the_stored_outcome() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = seeded_quota(&storage, Some(100)).await;
    let original = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(30, &original)
        .await
        .expect("debit");
    let rollback = idem(OperationType::Rollback, "r1", 2);
    storage
        .apply_rollback(&ctx(), &scope(), &target(&original), &rollback, &[])
        .await
        .expect("first rollback");

    let replay = storage
        .apply_rollback(&ctx(), &scope(), &target(&original), &rollback, &[])
        .await
        .expect("replay");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));
    assert_eq!(storage.consumed(id), 0);

    let mismatch = IdempotencyWrite {
        payload_hash: PayloadHash::from_bytes([7; 32]),
        ..rollback.clone()
    };
    let err = storage
        .apply_rollback(&ctx(), &scope(), &target(&original), &mismatch, &[])
        .await
        .expect_err("a different payload under the same key");
    assert_eq!(err, StorageError::IdempotencyPayloadMismatch);
}

#[tokio::test]
async fn a_snapshot_read_materializes_the_current_row_and_settles_nothing() {
    let (storage, id) = consumption_storage(Some(100), Vec::new()).await;
    storage
        .apply_debit_plan_for(10, &idem(OperationType::Debit, "k1", 1))
        .await
        .expect("debit");
    let events_before = storage.events().len();

    storage.set_now(Some(at("2026-03-19T00:00:00Z")));
    let snapshots = storage
        .read_quota_snapshot(&ctx(), &scope(), &applicable())
        .await
        .expect("read");

    assert_eq!(snapshots[0].consumed, 0, "the new period starts empty");
    let rows = storage.period_rows(id);
    assert_eq!(rows.len(), 2, "the successor row was materialized");
    assert!(!rows[0].settled, "a read settles nothing");
    assert_eq!(
        storage.events().len(),
        events_before,
        "a read enqueues nothing, so a preview never writes an outbox row"
    );

    // Repeating the read writes nothing further.
    storage
        .read_quota_snapshot(&ctx(), &scope(), &applicable())
        .await
        .expect("read again");
    assert_eq!(storage.period_rows(id).len(), 2);

    // The next mutation is what settles and emits.
    storage
        .apply_debit_plan_for(1, &idem(OperationType::Debit, "k2", 2))
        .await
        .expect("debit");
    assert!(storage.period_rows(id)[0].settled);
    assert!(kinds(&storage).contains(&NotificationEventKind::PeriodRollover));
}

#[tokio::test]
async fn a_replay_after_the_retention_window_is_a_new_operation() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = seeded_quota(&storage, Some(100)).await;
    let write = idem(OperationType::Debit, "k1", 1);
    storage
        .apply_debit_plan_for(10, &write)
        .await
        .expect("debit");

    let reclaimed = storage
        .reclaim_expired_idempotency(10, at("2999-01-01T00:00:00Z"))
        .await
        .expect("reclaim");
    assert_eq!(reclaimed, 1);

    storage
        .apply_debit_plan_for(10, &write)
        .await
        .expect("re-evaluated as a new operation");
    assert_eq!(storage.consumed(id), 20, "the counter moved a second time");
}

#[tokio::test]
async fn a_failed_batch_item_discards_every_effect_of_the_items_before_it() {
    let storage = storage_with_policy().await;
    storage.set_now(Some(day_one()));
    let id = storage
        .create_quota(
            &ctx(),
            &scope(),
            super::consumption_quota_draft(
                test_subject("u1"),
                Some(100),
                PeriodType::Day,
                vec![50],
            ),
            &[],
        )
        .await
        .expect("create quota");
    let events_before = storage.events().len();
    let log_before = storage.operation_log_len();
    let evaluator = Arc::new(ScriptedEvaluator::new());
    let call = scripted(&evaluator);

    // The first item crosses 50 and produces a threshold event; the second
    // exceeds the cap and denies the envelope.
    let items = vec![batch_item(60), batch_item(90)];
    let envelope = idem(OperationType::Debit, "batch", 5);
    let outcome = storage
        .apply_batch_debit(
            &ctx(),
            &scope(),
            &EvaluatedBatch {
                envelope: &envelope,
                items: &items,
                user_projection: None,
                limits: limits(),
                evaluate: Arc::clone(&call),
            },
            &[],
        )
        .await
        .expect("a denied envelope is a successful call");

    assert!(
        outcome
            .get()
            .iter()
            .any(|item| matches!(item.decision.result, DecisionResult::Denied { .. })),
        "the envelope was denied"
    );
    assert_eq!(storage.consumed(id), 0, "no counter survived the refusal");
    assert_eq!(
        storage.events().len(),
        events_before,
        "the threshold event the first item produced was discarded too"
    );
    assert_eq!(
        storage.operation_log_len(),
        log_before,
        "an uncommitted envelope writes no operation-log row"
    );
    assert!(
        storage.period_rows(id).iter().all(|row| row.consumed == 0),
        "the period row the first item filled was rolled back"
    );
}

// --- lease accounting under expiry -----------------------------------------

#[tokio::test]
async fn an_expired_hold_is_returned_once_by_whoever_touches_the_counter_first() {
    // The sequence that a read-side correction alone gets wrong: a credit that
    // floors, a fresh debit, and only then the sweeper. If the sweeper returned
    // the hold a second time it would erase the debit that came after it.
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(1000)).await;

    storage
        .apply_debit_plan_for(20, &idem(OperationType::Debit, "d1", 1))
        .await
        .expect("debit");
    let held = storage
        .acquire_lease_for(
            80,
            Duration::from_mins(1),
            &idem(OperationType::Reserve, "r1", 1),
        )
        .await
        .expect("acquire");
    let token = token_of(&held);
    assert_eq!(storage.consumed(id), 100, "20 debited plus 80 held");

    storage.expire_leases();
    assert_eq!(
        storage.consumed(id),
        20,
        "an expired hold stops counting the moment its TTL passes (I4)"
    );

    storage
        .apply_credit(&ctx(), &scope(), id, 50, &partial("c1", 2), &[])
        .await
        .expect("credit");
    assert_eq!(storage.consumed(id), 0, "a credit floors at zero");

    storage
        .apply_debit_plan_for(30, &idem(OperationType::Debit, "d2", 3))
        .await
        .expect("debit after the credit");
    assert_eq!(storage.consumed(id), 30);

    let reclaimed = storage
        .reclaim_expired_leases(10, OffsetDateTime::now_utc())
        .await
        .expect("sweep");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(storage.lease_state(token), Some(LeaseState::AutoReleased));
    assert_eq!(
        storage.consumed(id),
        30,
        "the hold was already returned, so the sweep moves nothing"
    );
    assert!(kinds(&storage).contains(&NotificationEventKind::LeaseAutoReleased));
}

#[tokio::test]
async fn an_expired_hold_frees_the_cap_and_the_capacity_without_a_sweep() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let ttl = Duration::from_mins(1);

    storage
        .acquire_lease_for(100, ttl, &idem(OperationType::Reserve, "r1", 1))
        .await
        .expect("the whole cap is held");
    assert_eq!(storage.consumed(id), 100);
    storage.expire_leases();

    // No sweeper has run: the row still reads active, and the capacity is free.
    let after = storage
        .acquire_lease_for(100, ttl, &idem(OperationType::Reserve, "r2", 2))
        .await
        .expect("acquire against the expired hold's capacity");
    assert!(
        after.get().token.is_some(),
        "an unreclaimed expired hold blocks nothing"
    );
    assert_eq!(storage.consumed(id), 100, "only the live hold is counted");
}

#[tokio::test]
async fn a_denial_is_a_verdict_even_when_the_active_lease_cap_is_full() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(10)).await;
    storage
        .bootstrap(&crate::models::BootstrapBundle {
            config_defaults: ConfigDefaults {
                max_active_leases: 1,
                ..ConfigDefaults::default()
            },
            ..crate::models::BootstrapBundle::foundation()
        })
        .await
        .expect("seed a cap of one");
    let ttl = Duration::from_mins(1);

    storage
        .acquire_lease_for(10, ttl, &idem(OperationType::Reserve, "r1", 1))
        .await
        .expect("the one lease the cap allows");

    // The engine refuses this on capacity. The cap is full too, but the caller
    // is owed the verdict it asked for, not a 429 about a different limit.
    let denied = storage
        .acquire_lease_for(5, ttl, &idem(OperationType::Reserve, "r2", 2))
        .await
        .expect("a denial is a successful call");
    assert!(matches!(
        denied.get().decision.result,
        DecisionResult::Denied { .. }
    ));
    assert!(denied.get().token.is_none());
    assert_eq!(storage.consumed(id), 10, "a denial holds nothing");
}

#[tokio::test]
async fn a_commit_of_zero_returns_every_hold_and_still_reverses_as_a_no_op() {
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    let acquired = storage
        .acquire_lease_for(
            40,
            Duration::from_mins(1),
            &idem(OperationType::Reserve, "r1", 1),
        )
        .await
        .expect("acquire");
    let token = token_of(&acquired);

    let committed = storage
        .commit_lease(
            &ctx(),
            &scope(),
            token,
            Some(0),
            &partial_idem("c1", 5),
            &[],
        )
        .await
        .expect("a job that used nothing commits nothing");
    assert!(matches!(committed, TransitionOutcome::Applied(_)));
    assert_eq!(storage.consumed(id), 0, "every hold came back");
    assert_eq!(storage.lease_state(token), Some(LeaseState::Committed));

    let replay = storage
        .commit_lease(
            &ctx(),
            &scope(),
            token,
            Some(0),
            &partial_idem("c1", 5),
            &[],
        )
        .await
        .expect("the same key replays its success");
    assert!(matches!(replay, TransitionOutcome::NoOp(_)));

    // The commit is a real operation, so its rollback succeeds; it simply has
    // nothing to move. A denied debit, which also records no movement, stays
    // irreversible.
    let reversed = storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &RollbackTarget {
                original: IdempotencyScope {
                    tenant_id: test_tenant(),
                    subject_key: IdempotencySubjectKey::from_bytes([1; 32]),
                    operation_type: OperationType::Commit,
                    key: "c1".to_owned(),
                },
                authorized: authorized(),
            },
            &idem(OperationType::Rollback, "rb", 6),
            &[],
        )
        .await
        .expect("a zero commit reverses as a no-op");
    assert!(matches!(reversed, TransitionOutcome::Applied(_)));
    assert_eq!(storage.consumed(id), 0);
}

#[tokio::test]
async fn a_rollback_reverses_the_namespace_it_names_not_the_other() {
    // A debit and a lease commit under the same key, subjects and tenant. Only
    // the operation type on the target tells the plugin which one to reverse.
    let storage = storage_with_policy().await;
    let id = seeded_quota(&storage, Some(100)).await;
    // The key the acquisition persisted: the commit reuses it, and the debit
    // under the same client key resolves to the same one.
    let subject_key = IdempotencySubjectKey::from_bytes([1; 32]);

    let acquired = storage
        .acquire_lease_for(
            30,
            Duration::from_mins(1),
            &idem(OperationType::Reserve, "shared", 1),
        )
        .await
        .expect("acquire");
    let token = token_of(&acquired);
    storage
        .commit_lease(
            &ctx(),
            &scope(),
            token,
            Some(30),
            &partial_idem("shared", 5),
            &[],
        )
        .await
        .expect("commit the whole hold");
    storage
        .apply_debit_plan_for(20, &idem(OperationType::Debit, "shared", 7))
        .await
        .expect("a debit under the same key");
    assert_eq!(storage.consumed(id), 50, "30 committed plus 20 debited");

    let scope_for = |operation| RollbackTarget {
        original: IdempotencyScope {
            tenant_id: test_tenant(),
            subject_key,
            operation_type: operation,
            key: "shared".to_owned(),
        },
        authorized: authorized(),
    };
    storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &scope_for(OperationType::Commit),
            &idem(OperationType::Rollback, "rb-commit", 8),
            &[],
        )
        .await
        .expect("reverse the commit");
    assert_eq!(storage.consumed(id), 20, "only the commit was reversed");

    storage
        .apply_rollback(
            &ctx(),
            &scope(),
            &scope_for(OperationType::Debit),
            &idem(OperationType::Rollback, "rb-debit", 9),
            &[],
        )
        .await
        .expect("reverse the debit");
    assert_eq!(storage.consumed(id), 0);
}

#[tokio::test]
async fn a_settlement_refuses_a_token_outside_the_callers_tenant() {
    let storage = storage_with_policy().await;
    seeded_quota(&storage, Some(100)).await;
    let acquired = storage
        .acquire_lease_for(
            10,
            Duration::from_mins(1),
            &idem(OperationType::Reserve, "r1", 1),
        )
        .await
        .expect("acquire");
    let token = token_of(&acquired);

    let foreign = PartialIdempotencyWrite {
        tenant_id: crate::models::TenantId::from(Uuid::from_u128(0xdead_beef)),
        key: "c1".to_owned(),
        payload_hash: PayloadHash::from_bytes([5; 32]),
    };
    let refused = storage
        .commit_lease(&ctx(), &scope(), token, None, &foreign, &[])
        .await
        .expect_err("another tenant's token is simply absent");
    assert_eq!(refused, StorageError::LeaseNotFound { token });
}
