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
    EvaluatedMutation, IdempotencyScope, IdempotencyWrite, LeaseToken, MetricId, OperationType,
    PartialIdempotencyWrite, PayloadHash, PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft,
    QuotaId, StorageError, SubjectRef, TenantId, TransitionOutcome,
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
use toolkit_security::AccessScope;
use uuid::Uuid;

use quota_enforcement_storage_plugin::domain::ports::LeaseStore;
use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationOutbox, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_undelivered_outbox,
};

const USER_SUBJECT_TYPE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~";

fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x00ac_ce55))
}

fn scope() -> AccessScope {
    AccessScope::for_tenant(tenant().as_uuid())
}

fn actor() -> Actor {
    Actor::Caller {
        subject_id: Uuid::from_u128(0x5eed),
    }
}

fn subject(id: &str) -> SubjectRef {
    SubjectRef {
        subject_type: GtsTypeId::try_new(USER_SUBJECT_TYPE).expect("type id"),
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
/// otherwise, and deny with `NO_APPLICABLE_QUOTA` when none applies, as the
/// gear does. Deterministic, so a race's outcome is the backend's doing.
fn evaluator() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        if context.quotas.is_empty() {
            let none = Decision {
                result: DecisionResult::Denied {
                    violated_quota_ids: Vec::new(),
                    reason: quota_enforcement_sdk::NO_APPLICABLE_QUOTA.to_owned(),
                },
                debit_plan: Vec::new(),
                diagnostics: std::collections::BTreeMap::new(),
            };
            return Ok(EvaluationOutcome::validate(none, context)?);
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
                        vec![QuotaDebitPlan {
                            quota_id: quota.snapshot.quota_id,
                            amount: context.amount,
                        }]
                    })
                    .collect(),
            )
        } else {
            Decision {
                result: DecisionResult::Denied {
                    violated_quota_ids: exceeded,
                    reason: "QUOTA_EXCEEDED".to_owned(),
                },
                debit_plan: Vec::new(),
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
        let outbox = start_undelivered_outbox(db.clone()).await.expect("outbox");
        let bound = Arc::new(QeOutbox::new());
        bound.bind(Arc::clone(outbox.outbox())).expect("bind once");
        let enqueuer: Arc<dyn NotificationOutbox> = bound;
        let policies = SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer));
        policies
            .create_policy(
                &actor(),
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
                    &actor(),
                    &scope(),
                    &EvaluatedMutation {
                        applicable: &applicable,
                        amount,
                        request: &null,
                        metric_id: None,
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
            .read_quota_snapshot(&scope(), &applicable(subjects(&[holder])))
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
        metric_id: None,
        authorized: AttributionDigest::from_bytes([7; 32]),
        idempotency: IdempotencyWrite {
            scope: IdempotencyScope {
                tenant_id: tenant(),
                operation_type: OperationType::Debit,
                key: next_item_key(),
            },
            payload_hash: PayloadHash::from_bytes([1; 32]),
        },
    }
}

/// A key no other item of this process uses.
fn next_item_key() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "item-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
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
        let items = items
            .iter()
            .map(|(holder, amount)| item(holder, *amount))
            .collect();
        self.batch_of(items, key, evaluate, timeout)
    }

    /// [`Self::batch_with`] over items built by the caller.
    fn batch_of(
        &self,
        items: Vec<BatchDebitItem>,
        key: &str,
        evaluate: Arc<TransactionEvaluator>,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Batched> {
        let store = Arc::clone(&self.store);
        let key = key.to_owned();
        tokio::spawn(async move {
            let envelope = IdempotencyWrite {
                scope: IdempotencyScope {
                    tenant_id: tenant(),
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
                })
                .collect();
            store
                .apply_batch_debit(
                    &actor(),
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
                    &actor(),
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
                &actor(),
                &scope(),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount: 10,
                    request: &null,
                    metric_id: None,
                    limits: limits(),
                    idempotency: &IdempotencyWrite {
                        scope: IdempotencyScope {
                            tenant_id: tenant(),
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

/// Commit a batch envelope record under `key` without the scope lock: the
/// writer a batch can still lose the record's key to.
async fn commit_envelope_out_of_band(
    h: &PgHarness,
    key: &str,
    payload: u8,
    record: &quota_enforcement_sdk::BatchRecord,
) {
    use quota_enforcement_storage_plugin::infra::storage::entity::idempotency_record;
    use sea_orm::ActiveValue::Set;
    let now = OffsetDateTime::now_utc();
    let conn = h.db.conn().expect("conn");
    // Bounded: an insert that waits on the holder would otherwise deadlock
    // the test, which only releases the holder after this returns.
    let all = AccessScope::allow_all();
    let insert = toolkit_db::secure::secure_insert::<idempotency_record::Entity>(
        idempotency_record::ActiveModel {
            tenant_id: Set(tenant().as_uuid()),
            operation_type: Set(OperationType::BatchDebit.as_str().to_owned()),
            idem_key: Set(key.to_owned()),
            payload_hash: Set(vec![payload; 32]),
            decision_blob: Set(serde_json::to_string(record).expect("record")),
            engine_id: Set(None),
            policy_id: Set(None),
            policy_version: Set(None),
            created_at: Set(now),
            expires_at: Set(now + Duration::from_hours(1)),
        },
        &all,
        &conn,
    );
    tokio::time::timeout(NO_DEADLOCK, insert)
        .await
        .expect("the competing insert waited on the holder")
        .expect("commit the competing envelope");
}

fn winners_batch() -> quota_enforcement_sdk::BatchRecord {
    quota_enforcement_sdk::BatchRecord::new(vec![Decision {
        result: DecisionResult::Denied {
            violated_quota_ids: Vec::new(),
            reason: "WINNER".to_owned(),
        },
        debit_plan: Vec::new(),
        diagnostics: std::collections::BTreeMap::new(),
    }])
}

/// Start a one-item batch of 10 on `u1` under `key` that stops in its
/// evaluation, past its scope lock and envelope recheck.
fn hold_a_batch(
    h: &PgHarness,
    key: &str,
) -> (
    tokio::task::JoinHandle<Batched>,
    std::sync::mpsc::SyncSender<()>,
) {
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let batch = h.batch_with(
        &[("u1", 10)],
        key,
        held_evaluator(locked_tx, release_rx),
        Duration::from_secs(30),
    );
    locked_rx
        .recv_timeout(NO_DEADLOCK)
        .expect("the batch is evaluating");
    (batch, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_that_loses_the_envelope_key_replays_the_winner_and_applies_nothing() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let (batch, release) = hold_a_batch(&h, "raced");
    // The harness envelope payload is `[1; 32]`.
    commit_envelope_out_of_band(&h, "raced", 1, &winners_batch()).await;
    release.send(()).expect("release");

    match batch.await.expect("join") {
        Ok(TransitionOutcome::NoOp(replayed)) => {
            let decisions: Vec<Decision> = replayed.into_iter().map(|item| item.decision).collect();
            assert_eq!(
                decisions,
                winners_batch().decisions,
                "the winner's decisions"
            );
        }
        other => panic!("expected a replay of the winner, got {other:?}"),
    }
    assert_eq!(
        h.consumed("u1", id).await,
        0,
        "the loser's batch rolled back"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_that_loses_the_envelope_key_to_another_payload_is_a_mismatch() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let (batch, release) = hold_a_batch(&h, "raced");
    commit_envelope_out_of_band(&h, "raced", 2, &winners_batch()).await;
    release.send(()).expect("release");

    assert!(
        matches!(
            batch.await.expect("join"),
            Err(StorageError::IdempotencyPayloadMismatch)
        ),
        "the same key under another payload is the caller's conflict"
    );
    assert_eq!(h.consumed("u1", id).await, 0);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_debit_that_commits_first_under_an_item_key_refuses_the_batch() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    let debited = h.quota("u1", Some(100)).await;
    let batched = h.quota("u2", Some(100)).await;
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let store = Arc::clone(&h.store);
    let evaluate = held_evaluator(locked_tx, release_rx);
    let debit = tokio::spawn(async move {
        let applicable = applicable(subjects(&["u1"]));
        let null = serde_json::Value::Null;
        store
            .apply_debit_plan(
                &actor(),
                &scope(),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount: 10,
                    request: &null,
                    metric_id: None,
                    limits: limits(),
                    idempotency: &IdempotencyWrite {
                        scope: IdempotencyScope {
                            tenant_id: tenant(),
                            operation_type: OperationType::Debit,
                            key: "shared".to_owned(),
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
        .recv_timeout(NO_DEADLOCK)
        .expect("the debit holds the key lock of `shared`");

    // The batch's Quota is another one: only the item key's lock is shared.
    let mut shared = item("u2", 5);
    shared.idempotency.scope.key = "shared".to_owned();
    let batch = h.batch_of(
        vec![item("u2", 1), shared],
        "b1",
        evaluator(),
        Duration::from_secs(30),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    release_tx.send(()).expect("release");
    debit.await.expect("join").expect("the debit commits first");

    let outcome = tokio::time::timeout(NO_DEADLOCK, batch)
        .await
        .expect("no deadlock")
        .expect("join");
    assert!(
        outcome == Err(StorageError::IdempotencyPayloadMismatch.at_item(1)),
        "a batch never answers with a single debit's result, and names the item: {outcome:?}"
    );
    assert_eq!(h.consumed("u1", debited).await, 10);
    assert_eq!(
        h.consumed("u2", batched).await,
        0,
        "the batch changed nothing"
    );
    assert!(
        h.store
            .lookup_idempotency(&IdempotencyScope {
                tenant_id: tenant(),
                operation_type: OperationType::BatchDebit,
                key: "b1".to_owned(),
            })
            .await
            .expect("lookup")
            .is_none(),
        "no envelope record"
    );
    h.down().await;
}

// ---------------------------------------------------------------------------
// Quota conditions (I15): matched on the locked rows, per item
// ---------------------------------------------------------------------------

impl PgHarness {
    /// A Quota of `holder` whose metadata limits it to `region`.
    async fn regional_quota(&self, holder: &str, region: &str) -> QuotaId {
        let mut quota = draft(holder, Some(100));
        quota.metadata = serde_json::json!({ "region": [region] })
            .as_object()
            .cloned()
            .expect("object");
        self.quotas
            .create_quota(&actor(), &scope(), quota, &[])
            .await
            .expect("quota")
    }
}

fn regional_item(holder: &str, amount: u64, region: &str) -> BatchDebitItem {
    BatchDebitItem {
        request: serde_json::json!({ "region": region }),
        metric_id: None,
        ..item(holder, amount)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_batch_item_and_each_debit_reach_only_the_quotas_whose_conditions_hold() {
    let h = PgHarness::up().await;
    let eu = h.regional_quota("u1", "eu").await;
    let us = h.regional_quota("u1", "us").await;

    let batch = h
        .batch_of(
            vec![regional_item("u1", 3, "eu"), regional_item("u1", 5, "us")],
            "b1",
            evaluator(),
            Duration::from_secs(5),
        )
        .await
        .expect("join");
    assert!(admitted(&batch), "{batch:?}");
    assert_eq!(h.consumed("u1", eu).await, 3);
    assert_eq!(h.consumed("u1", us).await, 5);

    let debit = |region: Option<&str>, key: &str| {
        let store = Arc::clone(&h.store);
        let request = region.map_or(
            serde_json::json!({}),
            |region| serde_json::json!({ "region": region }),
        );
        let key = key.to_owned();
        async move {
            let applicable = applicable(subjects(&["u1"]));
            store
                .apply_debit_plan(
                    &actor(),
                    &scope(),
                    &EvaluatedMutation {
                        applicable: &applicable,
                        amount: 1,
                        request: &request,
                        metric_id: None,
                        limits: limits(),
                        idempotency: &IdempotencyWrite {
                            scope: IdempotencyScope {
                                tenant_id: tenant(),
                                operation_type: OperationType::Debit,
                                key,
                            },
                            payload_hash: PayloadHash::from_bytes([4; 32]),
                        },
                        authorized: AttributionDigest::from_bytes([7; 32]),
                        evaluate: evaluator(),
                    },
                    &[],
                )
                .await
                .expect("debit")
        }
    };
    debit(Some("eu"), "d1").await;
    assert_eq!(h.consumed("u1", eu).await, 4);
    assert_eq!(h.consumed("u1", us).await, 5);
    for (region, key) in [(Some("ap"), "d2"), (None, "d3")] {
        let outcome = debit(region, key).await;
        assert_eq!(
            outcome.get().decision.denied_reason(),
            Some(quota_enforcement_sdk::NO_APPLICABLE_QUOTA),
            "{region:?}"
        );
    }
    assert_eq!(h.consumed("u1", eu).await, 4);
    assert_eq!(h.consumed("u1", us).await, 5);
    h.down().await;
}
