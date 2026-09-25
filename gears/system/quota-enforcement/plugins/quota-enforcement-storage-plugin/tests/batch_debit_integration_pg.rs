#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed concurrency suite of the atomic batch debit.
//!
//! What only a real concurrent backend can show:
//!
//! - batches whose items name overlapping Quotas in opposite orders never
//!   deadlock, and no cap is exceeded;
//! - batches and single debits on one Quota share its cap exactly;
//! - a batch, and a single debit, hold their counter rows while they evaluate,
//!   so a lease release on one of them cannot interleave with the snapshot;
//! - an evaluation that outlasts the batch timer writes nothing.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test batch_debit_integration_pg`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AppliedMutation, AttributionDigest, Decision, DecisionResult, EvaluatedLease,
    EvaluatedMutation, IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, LeaseToken,
    MetricId, OperationType, PartialIdempotencyWrite, PayloadHash, PolicyDraft, PolicyScope,
    QuotaDebitPlan, QuotaDraft, QuotaId, StorageError, SubjectRef, TenantId, TransitionOutcome,
};
use quota_enforcement_sdk::{
    BatchDebitItem, BatchEntry, BatchTimer, EvaluatedBatch, EvaluatedDebit,
};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use quota_enforcement_storage_plugin::domain::ports::LeaseStore;
use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationEnqueuer, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_outbox,
};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_tokens_input.v1";

fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x00ac_ce55))
}

fn scope() -> AccessScope {
    AccessScope::for_tenant(tenant().as_uuid())
}

fn actor() -> Actor {
    Actor {
        subject_id: Uuid::from_u128(0x5eed),
        subject_type: None,
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

fn subject(id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::try_new(USER_PROJECTION).expect("type id"),
        subject_id: id.to_owned(),
    }
}

fn subjects(ids: &[&str]) -> Vec<SubjectRef> {
    ids.iter().map(|id| subject(id)).collect()
}

fn draft(subject_id: &str, cap: Option<u64>) -> QuotaDraft {
    let mut draft = quota_draft(subject(subject_id), cap);
    draft.tenant_id = tenant();
    draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    draft
}

fn applicable(subjects: Vec<SubjectRef>) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects,
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(500).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Allow the requested amount against every applicable Quota with room, deny
/// otherwise. Deterministic, so a race's outcome is the backend's doing.
fn evaluator() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
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
    })
}

async fn wait_for_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "postgres never listened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct PgHarness {
    db: toolkit_db::Db,
    store: Arc<SqlConsumptionStore>,
    quotas: SqlQuotaStore,
    outbox: OutboxHandle,
    _container: ContainerAsync<Postgres>,
}

type Acquired = Result<TransitionOutcome<EvaluatedLease>, StorageError>;
type Settled = Result<TransitionOutcome<AppliedMutation>, StorageError>;

impl PgHarness {
    async fn up() -> Self {
        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("mapped port");
        wait_for_tcp(port).await;
        let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
        let mut db = None;
        for _ in 0..20 {
            match connect_db(&dsn, ConnectOpts::default()).await {
                Ok(connected) => {
                    db = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let db = db.expect("connect to postgres");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        let outbox = start_outbox(db.clone()).await.expect("outbox");
        let bound = Arc::new(QeOutbox::new());
        bound.bind(Arc::clone(outbox.outbox())).expect("bind once");
        let enqueuer: Arc<dyn NotificationEnqueuer> = bound;
        let policies = SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer));
        policies
            .create_policy(
                &ctx(),
                PolicyDraft {
                    scope: PolicyScope::Global,
                    // Not the built-in engine, whose plans name one Quota only:
                    // the overlapping-sets test holds on two. Storage runs no
                    // engine itself; the test evaluator decides.
                    engine_id: "cel".to_owned(),
                    engine_config: serde_json::json!({}),
                    timeout_ms: Some(200),
                    description: None,
                    comment: None,
                    created_by: "test".to_owned(),
                    schema_snapshot: PolicySchemaSnapshot::default(),
                },
                &[],
            )
            .await
            .expect("seed the global policy");
        let clock = Arc::new(Mutex::new(OffsetDateTime::now_utc()));
        let store_reader = Arc::clone(&clock);
        let quota_reader = Arc::clone(&clock);
        Self {
            db: db.clone(),
            store: Arc::new(SqlConsumptionStore::with_clock(
                db.clone(),
                Arc::clone(&enqueuer),
                Arc::new(move || *store_reader.lock().expect("clock")),
            )),
            quotas: SqlQuotaStore::with_clock(
                db.clone(),
                Arc::clone(&enqueuer),
                Arc::new(move || *quota_reader.lock().expect("clock")),
            ),
            outbox,
            _container: container,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    /// A contention budget (I8) for the metric, so writers that meet on a row
    /// take turns instead of failing fast at the 0 ms default.
    async fn queue_writers(&self) {
        use quota_enforcement_storage_plugin::infra::storage::entity::contention_timeout_config;
        use sea_orm::ActiveValue::Set;
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<contention_timeout_config::Entity>(
            contention_timeout_config::ActiveModel {
                metric_key: Set(METRIC_TOKENS.to_owned()),
                timeout_ms: Set(10_000),
                updated_at: Set(OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the contention budget");
    }

    async fn quota(&self, subject_id: &str, cap: Option<u64>) -> QuotaId {
        self.quotas
            .create_quota(&actor(), &scope(), draft(subject_id, cap), &[])
            .await
            .expect("quota")
    }

    /// One acquisition over the Quotas of `holders`, spawned with owned
    /// inputs so it can race the others.
    fn acquire(
        &self,
        holders: &[&str],
        amount: u64,
        key: &str,
    ) -> tokio::task::JoinHandle<Acquired> {
        let store = Arc::clone(&self.store);
        let subjects = subjects(holders);
        let key = key.to_owned();
        tokio::spawn(async move {
            let applicable = applicable(subjects.clone());
            let idempotency = IdempotencyWrite {
                scope: IdempotencyScope {
                    tenant_id: tenant(),
                    subject_key: IdempotencySubjectKey::of(&subjects),
                    operation_type: OperationType::Reserve,
                    key,
                },
                payload_hash: PayloadHash::from_bytes(
                    [u8::try_from(amount).unwrap_or(u8::MAX); 32],
                ),
            };
            let null = serde_json::Value::Null;
            store
                .acquire_lease(
                    &ctx(),
                    &scope(),
                    &EvaluatedMutation {
                        applicable: &applicable,
                        amount,
                        request: &null,
                        resource: &null,
                        user_projection: None,
                        limits: limits(),
                        idempotency: &idempotency,
                        authorized: AttributionDigest::from_bytes([7; 32]),
                        evaluate: evaluator(),
                    },
                    Duration::from_mins(1),
                )
                .await
        })
    }

    /// Acquire and return the token, failing the test on anything else.
    async fn held(&self, holders: &[&str], amount: u64, key: &str) -> LeaseToken {
        let outcome = self.acquire(holders, amount, key).await.expect("join");
        token_of(&outcome).unwrap_or_else(|| panic!("not acquired: {outcome:?}"))
    }

    async fn consumed(&self, holder: &str, id: QuotaId) -> u64 {
        self.store
            .read_quota_snapshot(&ctx(), &scope(), &applicable(subjects(&[holder])))
            .await
            .expect("snapshot")
            .into_iter()
            .find(|snapshot| snapshot.quota_id == id)
            .map_or(0, |snapshot| snapshot.consumed)
    }
}

fn token_of(outcome: &Acquired) -> Option<LeaseToken> {
    match outcome {
        Ok(outcome) => outcome.get().token,
        Err(_) => None,
    }
}

type Batched = Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError>;

fn item(holder: &str, amount: u64) -> BatchDebitItem {
    BatchDebitItem {
        applicable: applicable(subjects(&[holder])),
        amount,
        request: serde_json::Value::Null,
        resource: serde_json::Value::Null,
        authorized: AttributionDigest::from_bytes([7; 32]),
        item_scope: None,
    }
}

impl PgHarness {
    /// One batch of `(holder, amount)` items under `key`, spawned so it can
    /// race the others, evaluated by `evaluate` on a timer of `timeout`.
    fn batch_with(
        &self,
        items: &[(&str, u64)],
        key: &str,
        evaluate: Arc<TransactionEvaluator>,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Batched> {
        let store = Arc::clone(&self.store);
        let items: Vec<BatchDebitItem> = items
            .iter()
            .map(|(holder, amount)| item(holder, *amount))
            .collect();
        let key = key.to_owned();
        tokio::spawn(async move {
            let holders: Vec<SubjectRef> = items
                .iter()
                .flat_map(|item| item.applicable.subjects.clone())
                .collect();
            let envelope = IdempotencyWrite {
                scope: IdempotencyScope {
                    tenant_id: tenant(),
                    subject_key: IdempotencySubjectKey::of(&holders),
                    operation_type: OperationType::BatchDebit,
                    key,
                },
                payload_hash: PayloadHash::from_bytes([1; 32]),
            };
            let item_scope = scope();
            let entries: Vec<BatchEntry<'_>> = items
                .iter()
                .map(|item| BatchEntry {
                    item,
                    scope: &item_scope,
                    user_projection: None,
                })
                .collect();
            store
                .apply_batch_debit(
                    &ctx(),
                    &scope(),
                    &EvaluatedBatch {
                        envelope: &envelope,
                        items: &entries,
                        limits: limits(),
                        evaluate,
                        timer: Arc::new(BatchTimer::new(timeout)),
                    },
                    &[],
                )
                .await
        })
    }

    fn batch(&self, items: &[(&str, u64)], key: &str) -> tokio::task::JoinHandle<Batched> {
        self.batch_with(items, key, evaluator(), Duration::from_secs(5))
    }

    fn release(&self, token: LeaseToken, key: &str) -> tokio::task::JoinHandle<Settled> {
        let store = Arc::clone(&self.store);
        let key = key.to_owned();
        tokio::spawn(async move {
            store
                .release_lease(
                    &ctx(),
                    &scope(),
                    token,
                    &PartialIdempotencyWrite {
                        tenant_id: tenant(),
                        key,
                        payload_hash: PayloadHash::from_bytes([3; 32]),
                    },
                    &[],
                )
                .await
        })
    }
}

fn admitted(outcome: &Batched) -> bool {
    matches!(outcome, Ok(outcome) if outcome
        .get()
        .iter()
        .all(|item| matches!(item.decision.result, DecisionResult::Allowed)))
}

/// An evaluator that stops inside the batch's transaction, holding its locks,
/// until the test lets it go.
fn held_evaluator(
    locked: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
) -> Arc<TransactionEvaluator> {
    let inner = evaluator();
    let release = Mutex::new(release);
    Arc::new(move |context: &EvaluationContext<'_>| {
        locked.send(()).ok();
        // Blocking, so it gives up its worker first.
        tokio::task::block_in_place(|| release.lock().expect("release").recv().ok());
        inner(context)
    })
}

/// A whole test body must finish in this time; a deadlock would not.
const NO_DEADLOCK: Duration = Duration::from_mins(1);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batches_over_overlapping_quotas_in_opposite_orders_never_deadlock_or_overrun() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    let ids = [
        h.quota("u1", Some(6)).await,
        h.quota("u2", Some(6)).await,
        h.quota("u3", Some(6)).await,
    ];
    // Each pair appears in both orders, so per-item locking could deadlock.
    let shapes: [[&str; 2]; 6] = [
        ["u1", "u2"],
        ["u2", "u1"],
        ["u2", "u3"],
        ["u3", "u2"],
        ["u3", "u1"],
        ["u1", "u3"],
    ];
    let racers: Vec<_> = (0..18)
        .map(|n| {
            let [a, b] = shapes[n % 6];
            (shapes[n % 6], h.batch(&[(a, 1), (b, 1)], &format!("b{n}")))
        })
        .collect();
    let mut expected = std::collections::HashMap::new();
    for (shape, racer) in racers {
        let outcome = tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join");
        assert!(
            outcome.is_ok(),
            "a refusal is a verdict, not an error: {outcome:?}"
        );
        if admitted(&outcome) {
            for holder in shape {
                *expected.entry(holder).or_insert(0_u64) += 1;
            }
        }
    }
    for (holder, id) in ["u1", "u2", "u3"].into_iter().zip(ids) {
        let consumed = h.consumed(holder, id).await;
        assert_eq!(
            consumed,
            expected.get(holder).copied().unwrap_or(0),
            "{holder}"
        );
        assert!(consumed <= 6, "{holder} over its cap: {consumed}");
    }
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batches_and_single_debits_share_one_cap_exactly() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    let id = h.quota("u1", Some(10)).await;
    let mut batches = Vec::new();
    let mut singles = Vec::new();
    for n in 0..6 {
        batches.push(h.batch(&[("u1", 1), ("u1", 1)], &format!("b{n}")));
        singles.push(h.batch(&[("u1", 1)], &format!("s{n}")));
    }
    let mut taken = 0;
    for racer in batches {
        let outcome = tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join");
        if admitted(&outcome) {
            taken += 2;
        }
    }
    for racer in singles {
        let outcome = tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join");
        if admitted(&outcome) {
            taken += 1;
        }
    }
    assert_eq!(
        h.consumed("u1", id).await,
        taken,
        "every admitted unit, once"
    );
    assert!(taken <= 10, "the cap held: {taken}");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_release_cannot_change_a_counter_while_a_batch_evaluates_it() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let token = h.held(&["u1"], 30, "lease").await;
    assert_eq!(h.consumed("u1", id).await, 30);

    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let batch = h.batch_with(
        &[("u1", 10)],
        "held",
        held_evaluator(locked_tx, release_rx),
        Duration::from_secs(30),
    );
    locked_rx
        .recv()
        .expect("the batch is evaluating, holding its rows");

    // At the 0 ms default the release is refused on the counter row the
    // batch holds; without that lock it would lower the counter under the
    // batch's snapshot.
    let refused = h.release(token, "r1").await.expect("join");
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "the batch holds the counter row: {refused:?}"
    );

    release_tx.send(()).expect("release");
    let outcome = batch.await.expect("join");
    assert!(admitted(&outcome), "{outcome:?}");
    assert_eq!(
        h.consumed("u1", id).await,
        40,
        "the lease's 30 plus the batch's 10"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evaluation_that_outlasts_the_batch_timer_writes_nothing() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let inner = evaluator();
    let slow: Arc<TransactionEvaluator> = Arc::new(move |context: &EvaluationContext<'_>| {
        tokio::task::block_in_place(|| std::thread::sleep(Duration::from_millis(400)));
        inner(context)
    });

    let outcome = h
        .batch_with(&[("u1", 1)], "slow", slow, Duration::from_millis(100))
        .await
        .expect("join");

    assert!(
        matches!(outcome, Err(StorageError::BatchTimeout)),
        "{outcome:?}"
    );
    assert_eq!(h.consumed("u1", id).await, 0);
    assert!(
        h.store
            .lookup_idempotency(&IdempotencyScope {
                tenant_id: tenant(),
                subject_key: IdempotencySubjectKey::of(&subjects(&["u1"])),
                operation_type: OperationType::BatchDebit,
                key: "slow".to_owned(),
            })
            .await
            .expect("lookup")
            .is_none(),
        "no record: the retry runs again"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_release_cannot_change_a_counter_while_a_debit_evaluates_it() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let token = h.held(&["u1"], 30, "lease").await;

    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let store = Arc::clone(&h.store);
    let evaluate = held_evaluator(locked_tx, release_rx);
    let debit = tokio::spawn(async move {
        let applicable = applicable(subjects(&["u1"]));
        let null = serde_json::Value::Null;
        store
            .apply_debit_plan(
                &ctx(),
                &scope(),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount: 10,
                    request: &null,
                    resource: &null,
                    user_projection: None,
                    limits: limits(),
                    idempotency: &IdempotencyWrite {
                        scope: IdempotencyScope {
                            tenant_id: tenant(),
                            subject_key: IdempotencySubjectKey::of(&subjects(&["u1"])),
                            operation_type: OperationType::Debit,
                            key: "d1".to_owned(),
                        },
                        payload_hash: PayloadHash::from_bytes([4; 32]),
                    },
                    authorized: AttributionDigest::from_bytes([7; 32]),
                    evaluate,
                },
                &[],
            )
            .await
    });
    locked_rx
        .recv()
        .expect("the debit is evaluating, holding its rows");

    let refused = h.release(token, "r1").await.expect("join");
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "the debit holds the counter row: {refused:?}"
    );

    release_tx.send(()).expect("release");
    debit.await.expect("join").expect("the debit commits");
    assert_eq!(h.consumed("u1", id).await, 40);
    h.down().await;
}
