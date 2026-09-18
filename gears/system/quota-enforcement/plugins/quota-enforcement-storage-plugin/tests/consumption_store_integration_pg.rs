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

use std::sync::{Arc, Barrier, Mutex};
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
///
/// `gate`, when given, blocks here until every party arrives. The callback runs
/// inside the transaction, after its replay check and after its Quota rows are
/// locked, so a barrier here holds every writer in exactly the window the
/// record's primary key has to arbitrate. Without it the tasks may simply run
/// one after the other and the test would pass while proving nothing.
fn evaluator(gate: Option<Arc<Barrier>>) -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        if let Some(gate) = &gate {
            // A blocking wait on a Tokio worker: the suite runs multi-threaded
            // with more workers than parties, so this cannot starve.
            gate.wait();
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
        gate: Option<Arc<Barrier>>,
    ) -> tokio::task::JoinHandle<
        Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>, StorageError>,
    > {
        let store = Arc::clone(&self.store);
        let evaluate = evaluator(gate);
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
        let inner = evaluator(None);
        let release = Mutex::new(release);
        let evaluate: Arc<TransactionEvaluator> = Arc::new(move |context| {
            locked.send(()).ok();
            release.lock().expect("release").recv().ok();
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

    // Twenty writers of one unit each against a cap of ten. The row lock is
    // what makes exactly ten of them win.
    let mut tasks = Vec::new();
    for i in 0..20 {
        tasks.push(h.debit("u1", METRIC_TOKENS, 1, write(&format!("k{i}"), 1), None));
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
async fn one_scope_over_disjoint_quota_rows_is_arbitrated_by_the_record() {
    let h = PgHarness::up().await;
    // Two metrics, so the two writers lock different Quota rows and the row
    // locks cannot serialize them. Their idempotency scope is identical, and
    // the metric is part of the hashed payload, so their payloads differ.
    let tokens = h.quota("u1", METRIC_TOKENS, Some(100)).await;
    let other = h.quota("u1", METRIC_OTHER, Some(100)).await;

    let gate = Arc::new(Barrier::new(2));
    let first = h.debit(
        "u1",
        METRIC_TOKENS,
        5,
        write("shared", 1),
        Some(Arc::clone(&gate)),
    );
    let second = h.debit("u1", METRIC_OTHER, 5, write("shared", 2), Some(gate));
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
    assert_eq!(moved, 5, "the loser rolled back completely");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_concurrent_debits_commit_once_and_replay_once() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", METRIC_TOKENS, Some(100)).await;

    // Both writers lock the same Quota row, so the barrier has to sit where
    // only one of them can be: the second reaches its evaluation only after the
    // first commits and releases the row. Two parties with one arrival each
    // would deadlock, so the gate admits a single party and simply proves the
    // evaluation ran inside the transaction.
    let first = h.debit("u1", METRIC_TOKENS, 7, write("same", 1), None);
    let second = h.debit("u1", METRIC_TOKENS, 7, write("same", 1), None);
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
    h.debit("u1", METRIC_TOKENS, 10, write("k1", 1), None)
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
