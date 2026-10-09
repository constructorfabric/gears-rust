#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::engine::{EvaluationContext, EvaluationLimits, EvaluationOutcome};
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, BulkCreateEntry, BulkCreateEnvelope, BulkDeactivateEntry,
    BulkDeactivateEnvelope, BulkUpdateEntry, BulkUpdateEnvelope, CapPatch, Decision,
    EvaluatedMutation, IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, OperationType,
    PageRequest, PayloadHash, PolicyDraft, PolicyScope, Quota, QuotaDebitPlan, QuotaFilter,
    QuotaId, QuotaPatch, QuotaStatus, TransitionOutcome,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::Db;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::{AccessScope, SecurityContext};

use crate::domain::ports::{LeaseStore, QuotaStore, StoreError};
use crate::infra::storage::entity::{idempotency_record, lease, operation_log, quota};
use crate::infra::storage::repo::lease_repo::STATE_RESOLVED_BY_DEACTIVATION;
use crate::infra::storage::{SqlConsumptionStore, SqlPolicyStore, SqlQuotaStore};
use crate::test_support::{
    METRIC_TOKENS, actor, bound_outbox, count_rows, draft, enqueued_messages, other_tenant,
    quota_changed, scope_for, tenant, test_db, user,
};

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("context")
}

fn envelope_write(op: OperationType, key: &str, payload: u8) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&[]),
            operation_type: op,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([payload; 32]),
    }
}

fn create_entry(subject: &str) -> BulkCreateEntry {
    BulkCreateEntry {
        idempotency_key: Some(format!("seat-{subject}")),
        scope: scope_for(tenant()),
        draft: draft(tenant(), subject, Some(10)),
        events: vec![quota_changed(tenant())],
    }
}

fn update_entry(quota_id: QuotaId, cap: u64, scope: AccessScope) -> BulkUpdateEntry {
    BulkUpdateEntry {
        idempotency_key: None,
        scope,
        quota_id,
        patch: QuotaPatch {
            cap: Some(CapPatch::Bounded(cap)),
            ..QuotaPatch::default()
        },
        events: vec![quota_changed(tenant())],
    }
}

fn deactivate_entry(quota_id: QuotaId) -> BulkDeactivateEntry {
    BulkDeactivateEntry {
        idempotency_key: None,
        scope: scope_for(tenant()),
        quota_id,
        events: vec![quota_changed(tenant())],
    }
}

/// Allow the amount against every applicable Quota.
fn allow_all(context: &EvaluationContext<'_>) -> EvaluationOutcome {
    EvaluationOutcome::validate(
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
        ),
        context,
    )
    .expect("a plan over the applicable Quotas is valid")
}

struct Harness {
    db: Db,
    quotas: SqlQuotaStore,
    leases: SqlConsumptionStore,
    outbox: OutboxHandle,
}

impl Harness {
    async fn up() -> Self {
        let db = test_db().await;
        let (outbox, enqueuer) = bound_outbox(&db).await;
        SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer) as _)
            .create_policy(
                &ctx(),
                PolicyDraft {
                    scope: PolicyScope::Global,
                    engine_id: "cel".into(),
                    engine_config: serde_json::json!({}),
                    timeout_ms: Some(25),
                    description: None,
                    comment: None,
                    created_by: "test".into(),
                    schema_snapshot: quota_enforcement_sdk::engine::PolicySchemaSnapshot::default(),
                },
                &[],
            )
            .await
            .expect("seed the global policy");
        let quotas = SqlQuotaStore::new(db.clone(), Arc::clone(&enqueuer) as _);
        let leases = SqlConsumptionStore::new(db.clone(), enqueuer);
        Self {
            db,
            quotas,
            leases,
            outbox,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    async fn quota(&self, subject: &str, cap: Option<u64>) -> QuotaId {
        self.quotas
            .create_quota(
                &actor(),
                &scope_for(tenant()),
                draft(tenant(), subject, cap),
                &[],
            )
            .await
            .expect("create")
    }

    async fn get(&self, id: QuotaId) -> Quota {
        self.quotas
            .read_quotas(
                &AccessScope::allow_all(),
                QuotaFilter {
                    ids: vec![id],
                    ..QuotaFilter::default()
                },
                PageRequest::first(1),
            )
            .await
            .expect("read")
            .items
            .pop()
            .expect("present")
    }

    /// A live lease of `amount` over every Quota of `subject`.
    async fn lease(
        &self,
        subject: &str,
        amount: u64,
        key: &str,
    ) -> quota_enforcement_sdk::LeaseToken {
        let applicable = ApplicableQuotas {
            tenant_id: tenant(),
            subjects: vec![user(subject)],
            metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
        };
        let idempotency = IdempotencyWrite {
            scope: IdempotencyScope {
                tenant_id: tenant(),
                subject_key: IdempotencySubjectKey::of(&[user(subject)]),
                operation_type: OperationType::Reserve,
                key: key.to_owned(),
            },
            payload_hash: PayloadHash::from_bytes([3; 32]),
        };
        let null = serde_json::Value::Null;
        let outcome = self
            .leases
            .acquire_lease(
                &ctx(),
                &scope_for(tenant()),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &null,
                    resource: &null,
                    user_projection: None,
                    limits: EvaluationLimits {
                        upper_timeout_ms: std::num::NonZeroU64::new(50).expect("nonzero"),
                        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
                    },
                    idempotency: &idempotency,
                    authorized: AttributionDigest::from_bytes([7; 32]),
                    evaluate: Arc::new(|context: &EvaluationContext<'_>| Ok(allow_all(context))),
                },
                Duration::from_mins(10),
            )
            .await
            .expect("acquire")
            .into_inner();
        outcome.token.expect("a lease was issued")
    }

    async fn payload_types(&self) -> Vec<String> {
        enqueued_messages(&self.db)
            .await
            .into_iter()
            .map(|message| message.payload_type)
            .collect()
    }
}

#[tokio::test]
async fn a_bulk_create_writes_every_quota_its_side_rows_and_the_record_in_one_transaction() {
    let h = Harness::up().await;
    let envelope = BulkCreateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkCreateQuotas, "pack", 1),
        items: vec![create_entry("u1"), create_entry("u2")],
    };
    let outcome = h
        .quotas
        .bulk_create_quotas(&actor(), &envelope)
        .await
        .expect("bulk create");
    assert!(outcome.is_applied());
    let created = outcome.into_inner();
    assert_eq!(created.items.len(), 2);
    assert_eq!(created.items[1].idempotency_key.as_deref(), Some("seat-u2"));
    for item in &created.items {
        assert_eq!(h.get(item.quota_id).await.status, QuotaStatus::Active);
    }
    assert_eq!(count_rows::<operation_log::Entity>(&h.db).await, 2);
    assert_eq!(count_rows::<idempotency_record::Entity>(&h.db).await, 1);
    assert_eq!(h.payload_types().await, vec!["quota-changed"; 2]);

    let replay = h
        .quotas
        .bulk_create_quotas(&actor(), &envelope)
        .await
        .expect("replay");
    assert_eq!(replay, TransitionOutcome::NoOp(created));
    assert_eq!(
        count_rows::<quota::Entity>(&h.db).await,
        2,
        "a replay applies nothing"
    );
    assert_eq!(h.payload_types().await.len(), 2);

    let other_items = BulkCreateEnvelope {
        idempotency: envelope_write(OperationType::BulkCreateQuotas, "pack", 2),
        ..envelope
    };
    assert_eq!(
        h.quotas.bulk_create_quotas(&actor(), &other_items).await,
        Err(StoreError::IdempotencyPayloadMismatch)
    );
    h.down().await;
}

#[tokio::test]
async fn a_bulk_create_with_one_failing_item_writes_nothing() {
    let h = Harness::up().await;
    let mut foreign = create_entry("u2");
    foreign.draft.tenant_id = other_tenant();
    let envelope = BulkCreateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkCreateQuotas, "pack", 1),
        items: vec![create_entry("u1"), foreign],
    };
    assert_eq!(
        h.quotas.bulk_create_quotas(&actor(), &envelope).await,
        Err(StoreError::SubjectOutOfScope.at_item(1))
    );
    assert_eq!(count_rows::<quota::Entity>(&h.db).await, 0);
    assert_eq!(count_rows::<idempotency_record::Entity>(&h.db).await, 0);
    assert_eq!(h.payload_types().await, Vec::<String>::new());
    h.down().await;
}

#[tokio::test]
async fn a_cap_below_the_in_flight_amount_on_one_item_rolls_back_every_patch() {
    let h = Harness::up().await;
    let first = h.quota("u1", Some(10)).await;
    let held = h.quota("u2", Some(10)).await;
    h.lease("u2", 6, "hold").await;
    let envelope = BulkUpdateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkUpdateQuotas, "shrink", 1),
        items: vec![
            update_entry(first, 5, scope_for(tenant())),
            update_entry(held, 5, scope_for(tenant())),
        ],
    };
    assert_eq!(
        h.quotas.bulk_update_quotas(&actor(), &envelope).await,
        Err(StoreError::CapBelowConsumed {
            new_cap: 5,
            consumed: 6
        }
        .at_item(1))
    );
    assert_eq!(
        h.get(first).await.cap,
        Some(10),
        "the first patch rolled back"
    );
    assert_eq!(
        count_rows::<idempotency_record::Entity>(&h.db).await,
        1,
        "the lease's own"
    );
    h.down().await;
}

#[tokio::test]
async fn a_committed_bulk_update_replays_only_while_its_targets_stay_visible() {
    let h = Harness::up().await;
    let first = h.quota("u1", Some(10)).await;
    let second = h.quota("u2", Some(10)).await;
    let envelope = BulkUpdateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkUpdateQuotas, "raise", 1),
        items: vec![
            update_entry(first, 20, scope_for(tenant())),
            update_entry(second, 30, scope_for(tenant())),
        ],
    };
    let committed = h
        .quotas
        .bulk_update_quotas(&actor(), &envelope)
        .await
        .expect("bulk update")
        .into_inner();
    assert_eq!(
        committed
            .items
            .iter()
            .map(|item| (item.quota_id, item.record_version))
            .collect::<Vec<_>>(),
        vec![(first, 2), (second, 2)]
    );

    // A deactivated target stays readable, so the replay still answers.
    h.quotas
        .deactivate_quota(&actor(), &scope_for(tenant()), second, &[])
        .await
        .expect("deactivate");
    assert_eq!(
        h.quotas.bulk_update_quotas(&actor(), &envelope).await,
        Ok(TransitionOutcome::NoOp(committed))
    );

    // A scope narrowed since the commit hides the second target: not found,
    // never the stored outcome.
    let narrowed = BulkUpdateEnvelope {
        items: vec![
            update_entry(first, 20, scope_for(tenant())),
            update_entry(second, 30, scope_for(other_tenant())),
        ],
        ..envelope
    };
    assert_eq!(
        h.quotas.bulk_update_quotas(&actor(), &narrowed).await,
        Err(StoreError::QuotaNotFound { id: second }.at_item(1))
    );
    h.down().await;
}

#[tokio::test]
async fn a_visible_quota_of_another_tenant_is_not_found() {
    let h = Harness::up().await;
    let foreign = h
        .quotas
        .create_quota(
            &actor(),
            &scope_for(other_tenant()),
            draft(other_tenant(), "u1", Some(10)),
            &[],
        )
        .await
        .expect("create");
    let envelope = BulkUpdateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkUpdateQuotas, "raise", 1),
        items: vec![update_entry(foreign, 20, AccessScope::allow_all())],
    };
    assert_eq!(
        h.quotas.bulk_update_quotas(&actor(), &envelope).await,
        Err(StoreError::QuotaNotFound { id: foreign }.at_item(0))
    );
    h.down().await;
}

#[tokio::test]
async fn failures_are_reported_in_submission_order_not_lock_order() {
    let h = Harness::up().await;
    let low = h.quota("u1", Some(10)).await;
    h.quotas
        .deactivate_quota(&actor(), &scope_for(tenant()), low, &[])
        .await
        .expect("deactivate");
    // Generated later, so it sorts after `low`, and it names no row.
    let missing = QuotaId::generate();
    assert!(missing.as_uuid() > low.as_uuid());
    let envelope = BulkDeactivateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkDeactivateQuotas, "off", 1),
        items: vec![deactivate_entry(missing), deactivate_entry(low)],
    };
    assert_eq!(
        h.quotas.bulk_deactivate_quotas(&actor(), &envelope).await,
        Err(StoreError::QuotaNotFound { id: missing }.at_item(0))
    );
    h.down().await;
}

#[tokio::test]
async fn a_bulk_deactivate_resolves_a_shared_lease_under_the_earlier_item() {
    let h = Harness::up().await;
    let low = h.quota("u1", Some(10)).await;
    let high = h.quota("u1", Some(20)).await;
    assert!(low.as_uuid() < high.as_uuid());
    let lease = h.lease("u1", 4, "shared").await;
    let envelope = BulkDeactivateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkDeactivateQuotas, "off", 1),
        items: vec![deactivate_entry(high), deactivate_entry(low)],
    };
    let outcome = h
        .quotas
        .bulk_deactivate_quotas(&actor(), &envelope)
        .await
        .expect("bulk deactivate")
        .into_inner();
    assert_eq!(outcome.items[0].quota_id, high);
    assert_eq!(
        outcome.items[0].resolved_leases,
        vec![lease],
        "the earlier item"
    );
    assert_eq!(outcome.items[1].resolved_leases, Vec::new());
    for id in [low, high] {
        assert_eq!(h.get(id).await.status, QuotaStatus::Deactivated);
    }
    let conn = h.db.conn().expect("connection");
    let row = lease::Entity::find()
        .filter(lease::Column::Token.eq(lease.as_uuid()))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .expect("read lease")
        .expect("the lease row");
    assert_eq!(row.state, STATE_RESOLVED_BY_DEACTIVATION);
    let types = h.payload_types().await;
    assert_eq!(
        types.iter().filter(|kind| *kind == "quota-changed").count(),
        2
    );
    assert_eq!(
        types
            .iter()
            .filter(|kind| *kind == "lease-resolved-by-deactivation")
            .count(),
        1
    );
    h.down().await;
}
