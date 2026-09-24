#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed concurrency suite of the consumption store.
//!
//! Three properties only a real concurrent backend can show, because `SQLite`
//! serializes writers and cannot express two transactions contending:
//!
//! - the cap holds under concurrent debits, which is the row lock doing its job
//!   (I9, ADR-0002 acquisition order);
//! - two writers that share an idempotency scope but lock disjoint Quota rows
//!   are arbitrated by the record's primary key, not by the row locks;
//! - an update that waits out a period boundary reads the counter row a debit
//!   opened while it waited, so a cap can never be lowered below live usage
//!   (I6).
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test consumption_store_integration_pg`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, CapPatch, Decision, DecisionResult, EvaluatedMutation,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, MetricId, OperationType,
    PayloadHash, PeriodType, PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft, QuotaId,
    QuotaPatch, QuotaType, StorageError, SubjectRef, TenantId, TransitionOutcome,
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

use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationEnqueuer, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_outbox,
};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_tokens_input.v1";
const METRIC_OTHER: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1";

/// A Tuesday, and the day after it: one calendar period boundary apart.
const DAY_ONE: OffsetDateTime = time::macros::datetime!(2026-03-17 10:00:00 UTC);
const DAY_TWO: OffsetDateTime = time::macros::datetime!(2026-03-18 10:00:00 UTC);

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

fn draft(subject_id: &str, metric: &str, cap: Option<u64>) -> QuotaDraft {
    let mut draft = quota_draft(subject(subject_id), cap);
    draft.tenant_id = tenant();
    draft.metric = MetricId::parse(metric).expect("metric");
    draft
}

fn applicable(subject_id: &str, metric: &str) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects: vec![subject(subject_id)],
        metric: MetricId::parse(metric).expect("metric"),
    }
}

/// One idempotency scope, shared by every writer in the race tests.
fn write(key: &str, payload: u8) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&[subject("u1")]),
            operation_type: OperationType::Debit,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([payload; 32]),
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

/// What a debit task answers.
type DebitResult = Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>, StorageError>;

/// A debit stopped inside its transaction, and the sender that lets it go on.
type HeldDebit = (
    tokio::task::JoinHandle<DebitResult>,
    std::sync::mpsc::SyncSender<()>,
);

struct PgHarness {
    db: toolkit_db::Db,
    store: Arc<SqlConsumptionStore>,
    quotas: SqlQuotaStore,
    /// The consumption store's clock, so a test can move a period boundary.
    clock: Arc<Mutex<OffsetDateTime>>,
    /// The Quota store's own clock. Separate because the cap-guard test needs
    /// the update to read a different instant from the debit it races.
    update_clock: Arc<Mutex<OffsetDateTime>>,
    outbox: OutboxHandle,
    _container: ContainerAsync<Postgres>,
}

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
                    engine_id: "most-restrictive-wins".to_owned(),
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
        let update_clock = Arc::new(Mutex::new(OffsetDateTime::now_utc()));
        let store_reader = Arc::clone(&clock);
        let update_reader = Arc::clone(&update_clock);
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
                Arc::new(move || *update_reader.lock().expect("clock")),
            ),
            clock,
            update_clock,
            outbox,
            _container: container,
        }
    }

    fn set_now(&self, now: OffsetDateTime) {
        *self.clock.lock().expect("clock") = now;
    }

    /// Give `metric` a contention budget (I8). Without one the platform default
    /// applies: 0 ms, fail fast.
    async fn set_contention_timeout(&self, metric: &str, timeout: Duration) {
        use quota_enforcement_storage_plugin::infra::storage::entity::contention_timeout_config;
        use sea_orm::ActiveValue::Set;
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<contention_timeout_config::Entity>(
            contention_timeout_config::ActiveModel {
                metric_key: Set(metric.to_owned()),
                timeout_ms: Set(i64::try_from(timeout.as_millis()).expect("fits")),
                updated_at: Set(OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the contention budget");
    }

    fn set_update_now(&self, now: OffsetDateTime) {
        *self.update_clock.lock().expect("clock") = now;
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    async fn quota(&self, subject_id: &str, metric: &str, cap: Option<u64>) -> QuotaId {
        self.quotas
            .create_quota(&actor(), &scope(), draft(subject_id, metric, cap), &[])
            .await
            .expect("quota")
    }

    async fn consumption_quota(&self, subject_id: &str, cap: Option<u64>) -> QuotaId {
        let mut d = draft(subject_id, METRIC_TOKENS, cap);
        d.quota_type = QuotaType::Consumption;
        d.period = Some(PeriodType::Day);
        self.quotas
            .create_quota(&actor(), &scope(), d, &[])
            .await
            .expect("consumption quota")
    }

    /// One debit through the store, with its own owned inputs so the future is
    /// `'static` and can be spawned against the others.
    fn debit(
        &self,
        subject_id: &'static str,
        metric: &'static str,
        amount: u64,
        idempotency: IdempotencyWrite,
    ) -> tokio::task::JoinHandle<
        Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>, StorageError>,
    > {
        let store = Arc::clone(&self.store);
        let evaluate = evaluator();
        tokio::spawn(async move {
            let applicable = applicable(subject_id, metric);
            let null = serde_json::Value::Null;
            store
                .apply_debit_plan(
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
                        evaluate,
                    },
                    &[],
                )
                .await
        })
    }

    /// A debit that stops inside its transaction, after its Quota rows are
    /// locked and its counter row opened, until the test releases it.
    fn debit_held(
        &self,
        subject_id: &'static str,
        metric: &'static str,
        amount: u64,
        idempotency: IdempotencyWrite,
        locked: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<
        Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>, StorageError>,
    > {
        let store = Arc::clone(&self.store);
        let inner = evaluator();
        let release = Mutex::new(release);
        let evaluate: Arc<TransactionEvaluator> = Arc::new(move |context| {
            locked.send(()).ok();
            // Blocking, so it gives up its worker first (see `evaluator`).
            tokio::task::block_in_place(|| release.lock().expect("release").recv().ok());
            inner(context)
        });
        tokio::spawn(async move {
            let applicable = applicable(subject_id, metric);
            let null = serde_json::Value::Null;
            store
                .apply_debit_plan(
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
                        evaluate,
                    },
                    &[],
                )
                .await
        })
    }

    async fn consumed(&self, subject_id: &'static str, metric: &'static str, id: QuotaId) -> u64 {
        self.store
            .read_quota_snapshot(&ctx(), &scope(), &applicable(subject_id, metric))
            .await
            .expect("snapshot")
            .into_iter()
            .find(|snapshot| snapshot.quota_id == id)
            .map_or(0, |snapshot| snapshot.consumed)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_debits_never_exceed_the_cap() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(10)).await;
    // Twenty writers on one row need a budget to take turns; at the 0 ms
    // default all but the first would be refused, which the fail-fast test
    // below asserts on its own.
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_secs(10))
        .await;

    // Twenty writers of one unit each against a cap of ten. The row lock is
    // what makes exactly ten of them win.
    let mut tasks = Vec::new();
    for i in 0..20 {
        tasks.push(h.debit("u1", METRIC_TOKENS, 1, write(&format!("k{i}"), 1)));
    }
    let mut allowed = 0;
    for task in tasks {
        let outcome = task.await.expect("join").expect("debit");
        if outcome.get().decision.result == DecisionResult::Allowed {
            allowed += 1;
        }
    }

    assert_eq!(allowed, 10, "the cap held under twenty concurrent writers");
    assert_eq!(h.consumed("u1", METRIC_TOKENS, id).await, 10);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_scope_over_disjoint_quota_rows_is_serialized_by_the_scope_lock() {
    let h = PgHarness::up().await;
    // Two metrics, so the two writers lock different Quota rows and the row
    // locks cannot serialize them. Their idempotency scope is identical, and
    // the metric is part of the hashed payload, so their payloads differ.
    let tokens = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    let other = h.quota("u1", METRIC_OTHER, Some(100)).await;
    // The second writer waits out the first on the scope lock; at the 0 ms
    // default it is refused instead (tested below).
    for metric in [METRIC_TOKENS, METRIC_OTHER] {
        h.set_contention_timeout(metric, Duration::from_secs(10))
            .await;
    }

    let first = h.debit("u1", METRIC_TOKENS, 5, write("shared", 1));
    let second = h.debit("u1", METRIC_OTHER, 5, write("shared", 2));
    let (first, second) = (first.await.expect("join"), second.await.expect("join"));

    let mut applied = 0;
    let mut mismatched = 0;
    for outcome in [first, second] {
        match outcome {
            Ok(TransitionOutcome::Applied(_)) => applied += 1,
            Err(StorageError::IdempotencyPayloadMismatch) => mismatched += 1,
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(applied, 1, "exactly one writer owned the key");
    assert_eq!(
        mismatched, 1,
        "the loser saw the winner's differing payload"
    );

    let moved =
        h.consumed("u1", METRIC_TOKENS, tokens).await + h.consumed("u1", METRIC_OTHER, other).await;
    assert_eq!(moved, 5, "the loser moved nothing");
    h.down().await;
}

/// Start a debit of `shared` on `u1`'s tokens Quota that stops inside its
/// transaction, holding its scope lock, and wait until it is there.
fn hold_the_scope(h: &PgHarness) -> HeldDebit {
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let holder = h.debit_held(
        "u1",
        METRIC_TOKENS,
        5,
        write("shared", 1),
        locked_tx,
        release_rx,
    );
    locked_rx
        .recv()
        .expect("the holder is inside its transaction");
    (holder, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_the_default_budget_a_writer_of_the_same_key_over_other_quotas_fails_fast() {
    let h = PgHarness::up().await;
    let tokens = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    let other = h.quota("u1", METRIC_OTHER, Some(100)).await;
    let (holder, release) = hold_the_scope(&h);

    // No row lock is shared, so only the scope lock can stop this writer
    // before it meets the holder's record.
    let started = std::time::Instant::now();
    let refused = h
        .debit("u1", METRIC_OTHER, 5, write("shared", 2))
        .await
        .expect("join");
    let waited = started.elapsed();
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "the scope is held: refused at 0 ms, got {refused:?}"
    );
    assert!(waited < Duration::from_secs(1), "no queueing: {waited:?}");

    release.send(()).expect("release");
    assert!(matches!(
        holder.await.expect("join"),
        Ok(TransitionOutcome::Applied(_))
    ));
    assert_eq!(h.consumed("u1", METRIC_TOKENS, tokens).await, 5);
    assert_eq!(h.consumed("u1", METRIC_OTHER, other).await, 0);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_budget_bounds_a_wait_on_the_same_key() {
    let h = PgHarness::up().await;
    h.quota("u1", METRIC_TOKENS, Some(100)).await;
    h.quota("u1", METRIC_OTHER, Some(100)).await;
    h.set_contention_timeout(METRIC_OTHER, Duration::from_millis(200))
        .await;
    let (holder, release) = hold_the_scope(&h);
    // The holder outlasts the contender's budget several times over.
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        release.send(()).expect("release");
    });

    let started = std::time::Instant::now();
    let refused = h
        .debit("u1", METRIC_OTHER, 5, write("shared", 2))
        .await
        .expect("join");
    let waited = started.elapsed();
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "got {refused:?}"
    );
    assert!(
        waited >= Duration::from_millis(150),
        "it waited within its budget first: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(1),
        "the budget, not the holder, ended the wait: {waited:?}"
    );

    releaser.await.expect("join");
    assert!(matches!(
        holder.await.expect("join"),
        Ok(TransitionOutcome::Applied(_))
    ));
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_concurrent_debits_commit_once_and_replay_once() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    // The second writer has to wait out the first to replay its record; at
    // the 0 ms default it is refused instead (the fail-fast test's subject).
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_secs(10))
        .await;

    let first = h.debit("u1", METRIC_TOKENS, 7, write("same", 1));
    let second = h.debit("u1", METRIC_TOKENS, 7, write("same", 1));
    let outcomes = [
        first.await.expect("join").expect("debit"),
        second.await.expect("join").expect("debit"),
    ];

    let applied = outcomes
        .iter()
        .filter(|o| matches!(o, TransitionOutcome::Applied(_)))
        .count();
    let replayed = outcomes
        .iter()
        .filter(|o| matches!(o, TransitionOutcome::NoOp(_)))
        .count();
    assert_eq!((applied, replayed), (1, 1));
    assert_eq!(
        h.consumed("u1", METRIC_TOKENS, id).await,
        7,
        "the counter moved once"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cap_update_that_waits_out_a_boundary_sees_the_successor_period() {
    let h = PgHarness::up().await;
    let id = h.consumption_quota("u1", Some(100)).await;

    // Day one: the current period holds 10.
    h.set_now(DAY_ONE);
    h.debit("u1", METRIC_TOKENS, 10, write("k1", 1))
        .await
        .expect("join")
        .expect("first debit");

    // A debit on day two opens the successor row and stops inside its
    // transaction, holding the Quota row and its new counter row.
    let (locked, release) = (
        std::sync::mpsc::sync_channel::<()>(1),
        std::sync::mpsc::sync_channel::<()>(1),
    );
    h.set_now(DAY_TWO);
    let debit = h.debit_held("u1", METRIC_TOKENS, 60, write("k2", 2), locked.0, release.1);
    locked.1.recv().expect("the debit reached its evaluation");

    // The update starts while its own clock still reads day one, then blocks on
    // the Quota row the debit holds. Its clock moves to day two while it waits,
    // which is what a wall clock does to a transaction that waits out a
    // boundary. Sampling the clock before the lock would filter the successor
    // row out and read no consumption at all.
    h.set_update_now(DAY_ONE);
    let quotas = h.quotas.clone();
    let update = tokio::spawn(async move {
        quotas
            .update_quota(
                &actor(),
                &scope(),
                id,
                QuotaPatch {
                    cap: Some(CapPatch::Bounded(50)),
                    ..QuotaPatch::default()
                },
                &[],
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.set_update_now(DAY_TWO);
    release.0.send(()).expect("release the debit");

    debit.await.expect("join").expect("second debit");
    let refused = update
        .await
        .expect("join")
        .expect_err("the cap guard refuses");

    assert!(
        format!("{refused:?}").contains("CapBelowConsumed"),
        "the successor period had already consumed 60, got {refused:?}"
    );
    assert_eq!(h.consumed("u1", METRIC_TOKENS, id).await, 60);
    h.down().await;
}

// ---------------------------------------------------------------------------
// The contention budget (I8): NOWAIT locks and a deadline-bound retry
// ---------------------------------------------------------------------------

/// Start a debit that holds the Quota row of `u1` until released, and wait
/// until it has the lock.
fn hold_the_row(h: &PgHarness, key: &str) -> HeldDebit {
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let holder = h.debit_held("u1", METRIC_TOKENS, 1, write(key, 1), locked_tx, release_rx);
    locked_rx.recv().expect("the holder locks the row");
    (holder, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_the_default_budget_a_contended_debit_fails_fast() {
    let h = PgHarness::up().await;
    h.quota("u1", METRIC_TOKENS, Some(100)).await;
    let (holder, release) = hold_the_row(&h, "held");

    let started = std::time::Instant::now();
    let refused = h
        .debit("u1", METRIC_TOKENS, 1, write("contender", 2))
        .await
        .expect("join");
    let waited = started.elapsed();
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "the default budget is 0 ms: a held row is refused, got {refused:?}"
    );
    assert!(
        waited < Duration::from_secs(1),
        "fail fast means no queueing behind the holder: {waited:?}"
    );

    release.send(()).expect("release");
    holder.await.expect("join").expect("the holder commits");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_positive_budget_waits_out_the_holder_and_then_succeeds() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_secs(5))
        .await;
    let (holder, release) = hold_the_row(&h, "held");

    let started = std::time::Instant::now();
    let contender = h.debit("u1", METRIC_TOKENS, 1, write("contender", 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    release.send(()).expect("release");
    holder.await.expect("join").expect("the holder commits");

    let admitted = contender
        .await
        .expect("join")
        .expect("the retry takes the row");
    let waited = started.elapsed();
    assert_eq!(admitted.get().decision.result, DecisionResult::Allowed);
    assert!(
        waited >= Duration::from_millis(250),
        "it could only proceed once the holder let go: {waited:?}"
    );
    assert_eq!(
        h.consumed("u1", METRIC_TOKENS, id).await,
        2,
        "both debits landed"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_budget_bounds_the_whole_wait_not_each_retry() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_millis(300))
        .await;
    let (holder, release) = hold_the_row(&h, "held");

    let started = std::time::Instant::now();
    let refused = h
        .debit("u1", METRIC_TOKENS, 1, write("contender", 2))
        .await
        .expect("join");
    let waited = started.elapsed();
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "a holder that outlasts the budget is a contention timeout, got {refused:?}"
    );
    assert!(
        waited >= Duration::from_millis(250),
        "it retried for the budget: {waited:?}"
    );
    assert!(
        waited < Duration::from_millis(1500),
        "the budget is a deadline across every retry: {waited:?}"
    );

    release.send(()).expect("release");
    holder.await.expect("join").expect("the holder commits");
    assert_eq!(
        h.consumed("u1", METRIC_TOKENS, id).await,
        1,
        "the refused debit held nothing"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rollback_racing_replays_of_its_debit_never_deadlocks() {
    // A replaying debit locks the Quota, then the record; a rollback of the
    // same key used to lock the record, then the Quota. Under one lock order,
    // and with every lock NOWAIT, neither can wait on the other in a cycle.
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_secs(10))
        .await;
    h.debit("u1", METRIC_TOKENS, 3, write("original", 1))
        .await
        .expect("join")
        .expect("the debit commits");
    assert_eq!(h.consumed("u1", METRIC_TOKENS, id).await, 3);

    let target = quota_enforcement_sdk::RollbackTarget {
        original: write("original", 1).scope,
        authorized: AttributionDigest::from_bytes([7; 32]),
    };
    let mut rollbacks = Vec::new();
    let mut replays = Vec::new();
    for i in 0..6_u8 {
        let store = Arc::clone(&h.store);
        let target = target.clone();
        rollbacks.push(tokio::spawn(async move {
            let own = IdempotencyWrite {
                scope: IdempotencyScope {
                    operation_type: OperationType::Rollback,
                    key: format!("rollback-{i}"),
                    ..write("unused", 0).scope
                },
                payload_hash: PayloadHash::from_bytes([100 + i; 32]),
            };
            store
                .apply_rollback(&ctx(), &scope(), &target, &own, &[])
                .await
        }));
        replays.push(h.debit("u1", METRIC_TOKENS, 3, write("original", 1)));
    }
    for task in rollbacks {
        task.await
            .expect("join")
            .expect("a rollback completes; a deadlock would abort one");
    }
    for task in replays {
        let replayed = task
            .await
            .expect("join")
            .expect("a replay completes; a deadlock would abort one");
        assert!(
            matches!(replayed, TransitionOutcome::NoOp(_)),
            "the same key and payload replays"
        );
    }
    assert_eq!(
        h.consumed("u1", METRIC_TOKENS, id).await,
        0,
        "reversed exactly once, whichever rollback got there first"
    );
    h.down().await;
}

// ---------------------------------------------------------------------------
// Idempotency stripes: a fixed set of rows, so unrelated scopes can collide
// ---------------------------------------------------------------------------

/// A debit key other than `key` whose scope maps to the same stripe.
fn colliding_key(key: &str) -> String {
    use quota_enforcement_storage_plugin::infra::storage::repo::idempotency_repo::{
        ScopeKey, stripe_of,
    };
    let stripe = |key: &str| {
        let scope = write(key, 0).scope;
        stripe_of(&ScopeKey {
            tenant_id: scope.tenant_id.as_uuid(),
            subject_key: scope.subject_key.as_bytes(),
            operation_type: scope.operation_type.as_str(),
            idem_key: &scope.key,
        })
    };
    let target = stripe(key);
    (0..u32::MAX)
        .map(|n| format!("collide-{n}"))
        .find(|candidate| stripe(candidate) == target)
        .expect("some key shares the stripe")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unrelated_scope_on_a_held_stripe_is_refused_at_the_default_budget_and_waits_with_one() {
    let h = PgHarness::up().await;
    h.quota("u1", METRIC_TOKENS, Some(100)).await;
    let other = h.quota("u1", METRIC_OTHER, Some(100)).await;
    let (holder, release) = hold_the_scope(&h);
    // Another key over another Quota: nothing it touches is contended except
    // the stripe it shares with the holder's scope.
    let unrelated = colliding_key("shared");

    let refused = h
        .debit("u1", METRIC_OTHER, 5, write(&unrelated, 2))
        .await
        .expect("join");
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "a shared stripe is refused at 0 ms like a shared scope, got {refused:?}"
    );

    // With a budget it waits for the holder instead, and then goes through.
    h.set_contention_timeout(METRIC_OTHER, Duration::from_secs(10))
        .await;
    let waiting = h.debit("u1", METRIC_OTHER, 5, write(&unrelated, 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "it waits on the held stripe");
    release.send(()).expect("release");
    assert!(matches!(
        holder.await.expect("join"),
        Ok(TransitionOutcome::Applied(_))
    ));
    assert!(matches!(
        waiting.await.expect("join"),
        Ok(TransitionOutcome::Applied(_))
    ));
    assert_eq!(h.consumed("u1", METRIC_OTHER, other).await, 5);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_skips_a_record_whose_stripe_a_writer_holds() {
    let h = PgHarness::up().await;
    h.quota("u1", METRIC_TOKENS, Some(100)).await;
    h.quota("u1", METRIC_OTHER, Some(100)).await;
    // A record that expires, keyed so its stripe is the holder's.
    let expiring = colliding_key("shared");
    h.set_now(DAY_ONE);
    h.debit("u1", METRIC_OTHER, 1, write(&expiring, 1))
        .await
        .expect("join")
        .expect("the debit commits");
    let long_after = DAY_ONE + time::Duration::days(400);
    h.set_now(long_after);

    let (holder, release) = hold_the_scope(&h);
    let skipped = h
        .store
        .reclaim_expired_idempotency(100, long_after)
        .await
        .expect("reclaim");
    assert_eq!(skipped, 0, "the held stripe is skipped, not waited on");

    release.send(()).expect("release");
    holder.await.expect("join").expect("the holder commits");
    let reclaimed = h
        .store
        .reclaim_expired_idempotency(100, long_after)
        .await
        .expect("reclaim");
    assert_eq!(reclaimed, 1, "once the stripe is free the record goes");
    h.down().await;
}
