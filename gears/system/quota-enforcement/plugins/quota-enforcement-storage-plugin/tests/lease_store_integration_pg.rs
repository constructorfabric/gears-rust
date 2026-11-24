#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed concurrency suite of the lease store.
//!
//! What only a real concurrent backend can show, because `SQLite` serializes
//! writers and has no row locks:
//!
//! - an acquisition and a commit on the same Quota serialize, and the counter
//!   ends where the order they committed in says it should;
//! - a deactivation cascade and a commit of the same lease resolve it exactly
//!   once, without deadlocking;
//! - concurrent acquisitions admit exactly the active-lease cap, and an expired
//!   lease frees its slot without a sweep (I4, I7);
//! - two overlapping sweeps partition the expired leases, so each is
//!   reclaimed, and its capacity returned, once;
//! - acquisitions over overlapping sets of Quotas hold on every Quota of their
//!   plan or on none, and never deadlock (ADR-0002);
//! - settlement, the sweeper and the deactivation cascade never touch the
//!   capacity row, which only acquisitions lock, and the cap stays exact,
//!   also across distinct Quotas that share only that row;
//! - a sweep and a cascade crossing leases on the same Quotas take their
//!   counters in one order and never deadlock.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test lease_store_integration_pg`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Barrier;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AppliedMutation, AttributionDigest, BulkDeactivateEntry,
    BulkDeactivateEnvelope, Decision, DecisionResult, EvaluatedLease, EvaluatedMutation,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, LeaseToken, MetricId, OperationType,
    PartialIdempotencyWrite, PayloadHash, PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft,
    QuotaId, StorageError, SubjectRef, TenantId, TransitionOutcome,
};
use sea_orm_migration::MigratorTrait;
use testcontainers::core::ExecCommand;
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
use quota_enforcement_storage_plugin::infra::storage::config_cache::{
    ConfigCache, DEFAULT_REFRESH,
};
use quota_enforcement_storage_plugin::infra::storage::repo::lease_repo;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationOutbox, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_undelivered_outbox,
};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1";

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
    /// The one clock of both stores, so a test can move every lease past its
    /// expiry at once.
    clock: Arc<Mutex<OffsetDateTime>>,
    outbox: OutboxHandle,
    container: ContainerAsync<Postgres>,
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
        // One configuration cache for both stores, as the gear wires them.
        let config = Arc::new(ConfigCache::new(DEFAULT_REFRESH));
        Self {
            db: db.clone(),
            store: Arc::new(
                SqlConsumptionStore::with_clock(
                    db.clone(),
                    Arc::clone(&enqueuer),
                    Arc::new(move || *store_reader.lock().expect("clock")),
                )
                .with_config(Arc::clone(&config)),
            ),
            quotas: SqlQuotaStore::with_clock(
                db.clone(),
                Arc::clone(&enqueuer),
                Arc::new(move || *quota_reader.lock().expect("clock")),
            )
            .with_config(config),
            clock,
            outbox,
            container,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    fn now(&self) -> OffsetDateTime {
        *self.clock.lock().expect("clock")
    }

    fn advance(&self, by: Duration) {
        *self.clock.lock().expect("clock") += by;
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
        // The edit applies once the configuration snapshot refreshes.
        self.advance(DEFAULT_REFRESH);
    }

    /// The active-lease cap of this tenant and metric (I7).
    async fn cap_leases(&self, max: i32) {
        use quota_enforcement_storage_plugin::infra::storage::entity::lease_capacity_config;
        use sea_orm::ActiveValue::Set;
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<lease_capacity_config::Entity>(
            lease_capacity_config::ActiveModel {
                tenant_key: Set(tenant().as_uuid().to_string()),
                metric_key: Set(METRIC_TOKENS.to_owned()),
                max_active_leases: Set(max),
                updated_at: Set(OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the lease cap");
        // The edit applies once the configuration snapshot refreshes.
        self.advance(DEFAULT_REFRESH);
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
        self.acquire_with(holders, amount, key, evaluator())
    }

    /// [`Self::acquire`], evaluated by `evaluate`.
    fn acquire_with(
        &self,
        holders: &[&str],
        amount: u64,
        key: &str,
        evaluate: Arc<TransactionEvaluator>,
    ) -> tokio::task::JoinHandle<Acquired> {
        self.spawn_acquire(holders, amount, key, evaluate, None)
    }

    /// [`Self::acquire`], started once every racer reaches `start`.
    fn acquire_at(
        &self,
        holders: &[&str],
        amount: u64,
        key: &str,
        start: Arc<Barrier>,
    ) -> tokio::task::JoinHandle<Acquired> {
        self.spawn_acquire(holders, amount, key, evaluator(), Some(start))
    }

    fn spawn_acquire(
        &self,
        holders: &[&str],
        amount: u64,
        key: &str,
        evaluate: Arc<TransactionEvaluator>,
        start: Option<Arc<Barrier>>,
    ) -> tokio::task::JoinHandle<Acquired> {
        let store = Arc::clone(&self.store);
        let subjects = subjects(holders);
        let key = key.to_owned();
        tokio::spawn(async move {
            if let Some(start) = start {
                start.wait().await;
            }
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
                        evaluate,
                    },
                    Duration::from_mins(1),
                )
                .await
        })
    }

    /// `count` active leases an hour past their expiry, as a stalled sweeper leaves them.
    async fn seed_expired_leases(&self, count: u32) {
        let sql = format!(
            "INSERT INTO qe_leases (token, tenant_id, metric, subject_key, attribution_hash, \
             idem_key, state, reserved_amount, acquired_at, expiry_at, record_version, \
             created_at, updated_at) \
             SELECT gen_random_uuid(), '{tenant}', '{METRIC_TOKENS}', '\\x00', '\\x00', \
             'backlog-' || n, 'active', 1, now() - interval '2 hours', \
             now() - interval '1 hour', 1, now(), now() \
             FROM generate_series(1, {count}) AS n",
            tenant = tenant().as_uuid(),
        );
        self.psql_inserts(&sql, count).await;
    }

    /// `count` expired leases, each with an unreturned hold on `quota`, as a
    /// paused sweeper leaves them. A `period` row is created first when given.
    async fn seed_unreturned_holds(&self, quota: QuotaId, period: Option<Uuid>, count: u32) {
        let tenant = tenant().as_uuid();
        let quota = quota.as_uuid();
        let period_sql = period.map_or_else(String::new, |period| {
            format!(
                "INSERT INTO qe_quota_consumption_counters (period_id, quota_id, tenant_id, \
                 period_start, period_end, consumed, is_settled, record_version, created_at, \
                 updated_at) VALUES ('{period}', '{quota}', '{tenant}', \
                 now() - interval '2 days', now() - interval '1 day', 0, false, 1, now(), now()); "
            )
        });
        let period_value = period.map_or_else(|| "NULL".to_owned(), |period| format!("'{period}'"));
        let sql = format!(
            "{period_sql}WITH leases AS (INSERT INTO qe_leases (token, tenant_id, metric, \
             subject_key, attribution_hash, idem_key, state, reserved_amount, acquired_at, \
             expiry_at, record_version, created_at, updated_at) \
             SELECT gen_random_uuid(), '{tenant}', '{METRIC_TOKENS}', \
             decode(repeat('00', 32), 'hex'), decode(repeat('00', 32), 'hex'), \
             'backlog-' || n, 'active', 1, now() - interval '2 hours', \
             now() - interval '1 hour', 1, now(), now() \
             FROM generate_series(1, {count}) AS n RETURNING token) \
             INSERT INTO qe_lease_holds (lease_token, quota_id, tenant_id, held_amount, \
             period_id) SELECT token, '{quota}', '{tenant}', 1, {period_value} FROM leases"
        );
        self.psql_inserts(&sql, count).await;
    }

    /// Run `sql` through `psql` in the container and check it inserted `count` rows.
    async fn psql_inserts(&self, sql: &str, count: u32) {
        let args = [
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "user",
            "-d",
            "app",
            "-c",
            sql,
        ];
        let mut output = self
            .container
            .exec(ExecCommand::new(args.map(str::to_owned)))
            .await
            .expect("psql runs in the container");
        let stdout = output.stdout_to_vec().await.expect("psql output");
        assert!(
            String::from_utf8_lossy(&stdout).contains(&format!("INSERT 0 {count}")),
            "seeding failed: {}",
            String::from_utf8_lossy(&stdout)
        );
    }

    /// Acquire and return the token, failing the test on anything else.
    async fn held(&self, holders: &[&str], amount: u64, key: &str) -> LeaseToken {
        let outcome = self.acquire(holders, amount, key).await.expect("join");
        token_of(&outcome).unwrap_or_else(|| panic!("not acquired: {outcome:?}"))
    }

    fn commit(
        &self,
        token: LeaseToken,
        actual: u64,
        key: &str,
    ) -> tokio::task::JoinHandle<Settled> {
        let store = Arc::clone(&self.store);
        let key = key.to_owned();
        tokio::spawn(async move {
            store
                .commit_lease(
                    &ctx(),
                    &scope(),
                    token,
                    Some(actual),
                    &PartialIdempotencyWrite {
                        tenant_id: tenant(),
                        key,
                        payload_hash: PayloadHash::from_bytes([9; 32]),
                    },
                    &[],
                )
                .await
        })
    }

    fn release(&self, token: LeaseToken, key: &str) -> tokio::task::JoinHandle<Settled> {
        tokio::spawn(release_on(Arc::clone(&self.store), token, key.to_owned()))
    }

    /// Lock the pair's capacity row in an open transaction until the sender fires.
    async fn hold_capacity_row(
        &self,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), toolkit_db::DbError>>,
    ) {
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let db = self.db.clone();
        let holder = tokio::spawn(async move {
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let row = lease_repo::lock_capacity_row(
                        tx,
                        &AccessScope::allow_all(),
                        tenant().as_uuid(),
                        METRIC_TOKENS,
                        lease_repo::RowWait::Wait,
                    )
                    .await?;
                    assert!(row.is_some(), "the pair has its capacity row");
                    _ = locked_tx.send(());
                    _ = release_rx.await;
                    Ok::<_, toolkit_db::DbError>(())
                })
            })
            .await
        });
        locked_rx.await.expect("the capacity row is locked");
        (release_tx, holder)
    }

    /// Stamp one hold in an open transaction, rolled back when the sender fires.
    async fn hold_hold_row(
        &self,
        token: Uuid,
        quota: QuotaId,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), toolkit_db::DbError>>,
    ) {
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let db = self.db.clone();
        let now = self.now();
        let holder = tokio::spawn(async move {
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let stamped = lease_repo::mark_hold_returned(
                        tx,
                        &AccessScope::allow_all(),
                        token,
                        quota.as_uuid(),
                        now,
                    )
                    .await?;
                    assert!(stamped, "the hold is unreturned");
                    _ = locked_tx.send(());
                    _ = release_rx.await;
                    Err::<(), _>(toolkit_db::DbError::InvalidParameter(
                        "roll the stamp back".to_owned(),
                    ))
                })
            })
            .await
        });
        locked_rx.await.expect("the hold row is locked");
        (release_tx, holder)
    }

    /// Wait until exactly `count` backends queue on a row lock.
    async fn lock_waiters(&self, count: u32) {
        let args = [
            "psql",
            "-tA",
            "-U",
            "user",
            "-d",
            "app",
            "-c",
            "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'",
        ];
        for _ in 0..200 {
            let mut output = self
                .container
                .exec(ExecCommand::new(args.map(str::to_owned)))
                .await
                .expect("psql runs in the container");
            let stdout = output.stdout_to_vec().await.expect("psql output");
            if String::from_utf8_lossy(&stdout).trim() == count.to_string() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{count} lock waiters never appeared");
    }

    async fn live_leases(&self) -> u64 {
        let conn = self.db.conn().expect("conn");
        lease_repo::count_live(
            &conn,
            &AccessScope::allow_all(),
            tenant().as_uuid(),
            METRIC_TOKENS,
            self.now(),
        )
        .await
        .expect("count live leases")
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

    async fn state(&self, token: LeaseToken) -> String {
        let conn = self.db.conn().expect("conn");
        lease_repo::find(&conn, &AccessScope::allow_all(), token.as_uuid())
            .await
            .expect("read lease")
            .expect("lease exists")
            .state
    }

    async fn holds(&self, token: LeaseToken) -> usize {
        let conn = self.db.conn().expect("conn");
        lease_repo::holds_of(&conn, &AccessScope::allow_all(), token.as_uuid())
            .await
            .expect("read holds")
            .len()
    }
}

async fn release_on(store: Arc<SqlConsumptionStore>, token: LeaseToken, key: String) -> Settled {
    store
        .release_lease(
            &ctx(),
            &scope(),
            token,
            &PartialIdempotencyWrite {
                tenant_id: tenant(),
                key,
                payload_hash: PayloadHash::from_bytes([8; 32]),
            },
            &[],
        )
        .await
}

/// [`release_on`], retried past contention refusals so every admitted lease is released.
async fn release_settled(
    store: Arc<SqlConsumptionStore>,
    token: LeaseToken,
    key: String,
) -> Settled {
    for _ in 0..10 {
        let settled = release_on(Arc::clone(&store), token, key.clone()).await;
        if !matches!(settled, Err(StorageError::LeaseContentionTimeout)) {
            return settled;
        }
    }
    release_on(store, token, key).await
}

fn token_of(outcome: &Acquired) -> Option<LeaseToken> {
    match outcome {
        Ok(outcome) => outcome.get().token,
        Err(_) => None,
    }
}

/// A whole test body must finish in this time; a deadlock would not.
const NO_DEADLOCK: Duration = Duration::from_mins(1);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acquisition_and_a_commit_on_one_quota_serialize() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    for round in 0..12 {
        let holder = format!("u{round}");
        let id = h.quota(&holder, Some(100)).await;
        let first = h.held(&[&holder], 30, &format!("a{round}")).await;

        // Released commit: 30 held, 10 kept. The second acquisition fits only
        // if the commit returned its 20 first.
        let commit = h.commit(first, 10, &format!("c{round}"));
        let second = h.acquire(&[&holder], 80, &format!("b{round}"));
        let (commit, second) = tokio::time::timeout(NO_DEADLOCK, async {
            (commit.await.expect("join"), second.await.expect("join"))
        })
        .await
        .expect("no deadlock");

        assert!(
            matches!(commit, Ok(TransitionOutcome::Applied(_))),
            "{commit:?}"
        );
        let expected = if token_of(&second).is_some() { 90 } else { 10 };
        assert!(
            second.is_ok(),
            "a refusal is a verdict, not an error: {second:?}"
        );
        assert_eq!(h.consumed(&holder, id).await, expected, "round {round}");
    }
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cascade_and_a_commit_resolve_one_lease_exactly_once() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    for round in 0..12 {
        let holder = format!("d{round}");
        let id = h.quota(&holder, Some(100)).await;
        let token = h.held(&[&holder], 20, &format!("a{round}")).await;

        let commit = h.commit(token, 5, &format!("c{round}"));
        let quotas = h.quotas.clone();
        let cascade =
            tokio::spawn(async move { quotas.deactivate_quota(&actor(), &scope(), id, &[]).await });
        let (commit, cascade) = tokio::time::timeout(NO_DEADLOCK, async {
            (commit.await.expect("join"), cascade.await.expect("join"))
        })
        .await
        .expect("no deadlock");

        let resolved = cascade.expect("the deactivation commits").resolved_leases;
        match commit {
            Ok(TransitionOutcome::Applied(_)) => {
                assert!(
                    resolved.is_empty(),
                    "committed first, nothing left to resolve"
                );
                assert_eq!(h.state(token).await, "committed");
            }
            Err(StorageError::LeaseNotActive { .. }) => {
                assert_eq!(resolved, vec![token], "the cascade resolved it first");
                assert_eq!(h.state(token).await, "resolved_by_deactivation");
            }
            other => panic!("round {round}: unexpected commit outcome {other:?}"),
        }
    }
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_acquisitions_admit_exactly_the_lease_cap() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    h.cap_leases(3).await;
    let id = h.quota("u1", Some(1_000)).await;

    let racers: Vec<_> = (0..10)
        .map(|n| h.acquire(&["u1"], 1, &format!("k{n}")))
        .collect();
    let mut admitted = 0;
    let mut capped = 0;
    for racer in racers {
        match tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join")
        {
            Ok(outcome) if outcome.get().token.is_some() => admitted += 1,
            Err(StorageError::LeaseInflightLimitExceeded) => capped += 1,
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    assert_eq!(
        (admitted, capped),
        (3, 7),
        "the capacity row serializes the count"
    );
    assert_eq!(h.consumed("u1", id).await, 3);
    h.down().await;
}

/// Long enough for a transition that does not queue on the held row.
const UNBLOCKED: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_acquisition_waits_on_a_held_capacity_row() {
    let h = PgHarness::up().await;
    h.quota("u1", Some(1_000)).await;
    let deactivated = h.quota("u2", Some(1_000)).await;
    let bulk_deactivated = h.quota("u3", Some(1_000)).await;
    let expired = h.held(&["u1"], 10, "a1").await;
    h.advance(Duration::from_secs(61));
    let before = h.now();
    let committed = h.held(&["u1"], 10, "a2").await;
    let released = h.held(&["u1"], 10, "a3").await;
    let cascaded = h.held(&["u2"], 10, "a4").await;
    let bulk_cascaded = h.held(&["u3"], 10, "a7").await;

    let (unlock, holder) = h.hold_capacity_row().await;
    let commit = tokio::time::timeout(UNBLOCKED, h.commit(committed, 5, "c1"))
        .await
        .expect("the commit does not wait on the capacity row")
        .expect("join");
    assert!(
        matches!(commit, Ok(TransitionOutcome::Applied(_))),
        "{commit:?}"
    );
    let release = tokio::time::timeout(UNBLOCKED, h.release(released, "r1"))
        .await
        .expect("the release does not wait on the capacity row")
        .expect("join");
    assert!(
        matches!(release, Ok(TransitionOutcome::Applied(_))),
        "{release:?}"
    );
    let swept = tokio::time::timeout(UNBLOCKED, h.store.reclaim_expired_leases(10, before))
        .await
        .expect("the sweeper does not wait on the capacity row")
        .expect("reclaim");
    assert_eq!(
        swept.iter().map(|lease| lease.token).collect::<Vec<_>>(),
        vec![expired]
    );
    let cascade = tokio::time::timeout(
        UNBLOCKED,
        h.quotas
            .deactivate_quota(&actor(), &scope(), deactivated, &[]),
    )
    .await
    .expect("the cascade does not wait on the capacity row")
    .expect("the deactivation commits");
    assert_eq!(cascade.resolved_leases, vec![cascaded]);
    let bulk = BulkDeactivateEnvelope {
        tenant_id: tenant(),
        idempotency: IdempotencyWrite {
            scope: IdempotencyScope {
                tenant_id: tenant(),
                subject_key: IdempotencySubjectKey::of(&[]),
                operation_type: OperationType::BulkDeactivateQuotas,
                key: "b1".to_owned(),
            },
            payload_hash: PayloadHash::from_bytes([9; 32]),
        },
        items: vec![BulkDeactivateEntry {
            idempotency_key: None,
            scope: scope(),
            quota_id: bulk_deactivated,
            events: Vec::new(),
        }],
    };
    let bulk = tokio::time::timeout(UNBLOCKED, h.quotas.bulk_deactivate_quotas(&actor(), &bulk))
        .await
        .expect("the bulk cascade does not wait on the capacity row")
        .expect("the bulk deactivation commits");
    let TransitionOutcome::Applied(bulk) = bulk else {
        panic!("a fresh envelope applies: {bulk:?}");
    };
    assert_eq!(bulk.items[0].resolved_leases, vec![bulk_cascaded]);
    // The row still guards admission.
    let refused = tokio::time::timeout(UNBLOCKED, h.acquire(&["u1"], 1, "a5"))
        .await
        .expect("a NOWAIT acquisition gives up at once")
        .expect("join");
    assert!(
        matches!(refused, Err(StorageError::LeaseContentionTimeout)),
        "refused by the lock, not the cap: {refused:?}"
    );

    _ = unlock.send(());
    holder.await.expect("join").expect("the holder commits");
    assert!(token_of(&h.acquire(&["u1"], 1, "a6").await.expect("join")).is_some());
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cap_stays_exact_while_releases_interleave_with_acquisitions() {
    const CAP: u64 = 3;
    let h = Arc::new(PgHarness::up().await);
    h.queue_writers().await;
    h.cap_leases(i32::try_from(CAP).unwrap()).await;
    h.quota("u1", Some(1_000_000)).await;

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor = {
        let h = Arc::clone(&h);
        let done = Arc::clone(&done);
        tokio::spawn(async move {
            let mut most = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                most = most.max(h.live_leases().await);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            most
        })
    };
    let start = Arc::new(Barrier::new(40));
    let racers: Vec<_> = (0..40_u64)
        .map(|n| {
            let acquired = h.acquire_at(&["u1"], 1, &format!("k{n}"), Arc::clone(&start));
            let store = Arc::clone(&h.store);
            tokio::spawn(async move {
                let outcome = acquired.await.expect("join");
                if let Some(token) = token_of(&outcome) {
                    tokio::time::sleep(Duration::from_millis(n % 7)).await;
                    let settled = release_settled(store, token, format!("r{n}")).await;
                    assert!(
                        matches!(settled, Ok(TransitionOutcome::Applied(_))),
                        "{settled:?}"
                    );
                }
                outcome
            })
        })
        .collect();
    let mut admitted = 0;
    for racer in racers {
        match tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join")
        {
            Ok(outcome) if outcome.get().token.is_some() => admitted += 1,
            Err(
                StorageError::LeaseInflightLimitExceeded | StorageError::LeaseContentionTimeout,
            ) => {}
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let most = monitor.await.expect("join");
    assert!(admitted > CAP, "released slots are reused: {admitted}");
    assert!(most <= CAP, "{most} live leases against a cap of {CAP}");
    assert_eq!(
        h.live_leases().await,
        0,
        "every admitted lease was released"
    );
    Arc::into_inner(h)
        .expect("the harness is no longer shared")
        .down()
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_capacity_row_alone_holds_the_cap_across_distinct_quotas() {
    const CAP: u64 = 3;
    let h = Arc::new(PgHarness::up().await);
    h.queue_writers().await;
    h.cap_leases(i32::try_from(CAP).unwrap()).await;
    // One Quota per racer pair: no Quota row is shared, only the capacity row.
    let holders: Vec<String> = (0..12).map(|n| format!("q{n}")).collect();
    for holder in &holders {
        h.quota(holder, Some(1_000_000)).await;
    }

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let monitor = {
        let h = Arc::clone(&h);
        let done = Arc::clone(&done);
        tokio::spawn(async move {
            let mut most = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                most = most.max(h.live_leases().await);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            most
        })
    };
    let start = Arc::new(Barrier::new(48));
    let racers: Vec<_> = (0..48_u64)
        .map(|n| {
            let holder = &holders[usize::try_from(n).unwrap() % holders.len()];
            let acquired =
                h.acquire_at(&[holder.as_str()], 1, &format!("k{n}"), Arc::clone(&start));
            let h = Arc::clone(&h);
            tokio::spawn(async move {
                let outcome = acquired.await.expect("join");
                let mut seen = 0;
                if let Some(token) = token_of(&outcome) {
                    seen = h.live_leases().await;
                    tokio::time::sleep(Duration::from_millis(5 + n % 11)).await;
                    let settled =
                        release_settled(Arc::clone(&h.store), token, format!("r{n}")).await;
                    assert!(
                        matches!(settled, Ok(TransitionOutcome::Applied(_))),
                        "{settled:?}"
                    );
                }
                (outcome, seen)
            })
        })
        .collect();
    let (mut admitted, mut capped, mut refused, mut most_seen) = (0, 0, 0, 0);
    for racer in racers {
        let (outcome, seen) = tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join");
        most_seen = most_seen.max(seen);
        match outcome {
            Ok(outcome) if outcome.get().token.is_some() => admitted += 1,
            Err(StorageError::LeaseInflightLimitExceeded) => capped += 1,
            // NOWAIT refusals past the budget are an allowed answer here.
            Err(StorageError::LeaseContentionTimeout) => refused += 1,
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let most = monitor.await.expect("join").max(most_seen);
    eprintln!("admitted {admitted}, capped {capped}, refused {refused}, most live {most}");
    assert!(admitted > CAP, "released slots are reused: {admitted}");
    assert!(most <= CAP, "{most} live leases against a cap of {CAP}");
    assert_eq!(
        h.live_leases().await,
        0,
        "every admitted lease was released"
    );
    Arc::into_inner(h)
        .expect("the harness is no longer shared")
        .down()
        .await;
}

/// The sweep meets the high counter first, the cascade's spanning lease the low one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sweep_and_a_cascade_over_two_quotas_of_one_pair_do_not_deadlock() {
    let h = PgHarness::up().await;
    let first = h.quota("u1", Some(1_000)).await;
    let second = h.quota("u2", Some(1_000)).await;
    let ((low, low_holder), (high, high_holder)) = if first.as_uuid() < second.as_uuid() {
        ((first, "u1"), (second, "u2"))
    } else {
        ((second, "u2"), (first, "u1"))
    };
    // Live first: acquiring later would return the expired holds itself (I4).
    let spanning = h.held(&[low_holder, high_holder], 10, "l1").await;
    h.seed_unreturned_holds(high, None, 1).await;
    h.seed_unreturned_holds(low, None, 1).await;
    let before = h.now();
    let conn = h.db.conn().expect("conn");
    let on_high = lease_repo::unreturned_expired_holds(
        &conn,
        &AccessScope::allow_all(),
        high.as_uuid(),
        None,
        before,
    )
    .await
    .expect("expired holds")
    .first()
    .expect("the seeded hold")
    .lease_token;

    // The sweep stops on its first hold, holding whatever counters it took so far.
    let (unblock, side) = h.hold_hold_row(on_high, high).await;
    let store = Arc::clone(&h.store);
    let sweep = tokio::spawn(async move { store.reclaim_expired_leases(10, before).await });
    h.lock_waiters(1).await;
    let quotas = h.quotas.clone();
    let cascade =
        tokio::spawn(async move { quotas.deactivate_quota(&actor(), &scope(), low, &[]).await });
    h.lock_waiters(2).await;
    _ = unblock.send(());
    assert!(side.await.expect("join").is_err(), "the stamp rolls back");

    let (sweep, cascade) = tokio::time::timeout(NO_DEADLOCK, async {
        (sweep.await.expect("join"), cascade.await.expect("join"))
    })
    .await
    .expect("no hang");
    let swept = sweep.expect("the sweep commits");
    assert_eq!(
        swept
            .iter()
            .map(|lease| lease.holds.iter().map(|hold| hold.quota_id).collect())
            .collect::<Vec<Vec<_>>>(),
        vec![vec![high], vec![low]],
        "the high hold's lease expired first"
    );
    let cascade = cascade.expect("the deactivation commits");
    assert_eq!(cascade.resolved_leases, vec![spanning]);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_lease_frees_its_cap_slot_and_its_capacity_without_a_sweep() {
    let h = PgHarness::up().await;
    h.cap_leases(1).await;
    let id = h.quota("u1", Some(100)).await;
    h.held(&["u1"], 10, "a1").await;
    // The Quota has room, so the engine allows and the cap is what refuses.
    let full = h.acquire(&["u1"], 1, "a2").await.expect("join");
    assert!(
        matches!(full, Err(StorageError::LeaseInflightLimitExceeded)),
        "{full:?}"
    );

    // Past the 60 s TTL: no sweeper has run.
    h.advance(Duration::from_secs(61));
    let token = h.held(&["u1"], 10, "a3").await;
    assert_eq!(h.state(token).await, "active");
    assert_eq!(h.consumed("u1", id).await, 10, "only the live hold counts");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_overlapping_sweeps_reclaim_each_expired_lease_once() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let mut tokens = Vec::new();
    for n in 0..6 {
        tokens.push(h.held(&["u1"], 5, &format!("k{n}")).await);
    }
    h.advance(Duration::from_secs(61));
    let before = h.now();

    let sweeps: Vec<_> = (0..2)
        .map(|_| {
            let store = Arc::clone(&h.store);
            tokio::spawn(async move { store.reclaim_expired_leases(10, before).await })
        })
        .collect();
    let mut reclaimed = Vec::new();
    for sweep in sweeps {
        let expired = tokio::time::timeout(NO_DEADLOCK, sweep)
            .await
            .expect("no deadlock")
            .expect("join")
            .expect("reclaim");
        reclaimed.extend(expired.into_iter().map(|lease| lease.token));
    }

    // Each lease came back from exactly one sweep, which is also the one that
    // enqueued its single `lease-auto-released` event.
    reclaimed.sort();
    tokens.sort();
    assert_eq!(reclaimed, tokens, "partitioned, none twice, none missed");
    for token in &tokens {
        assert_eq!(h.state(*token).await, "auto_released");
    }
    assert_eq!(
        h.consumed("u1", id).await,
        0,
        "capacity returned once, not twice"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acquisitions_over_overlapping_quota_sets_hold_all_or_nothing() {
    let h = PgHarness::up().await;
    h.queue_writers().await;
    let ids = [
        h.quota("u1", Some(1_000)).await,
        h.quota("u2", Some(1_000)).await,
        h.quota("u3", Some(1_000)).await,
    ];
    let sets: [&[&str]; 3] = [&["u1", "u2"], &["u2", "u3"], &["u3", "u1"]];

    let racers: Vec<_> = (0..12)
        .map(|n| h.acquire(sets[n % 3], 1, &format!("k{n}")))
        .collect();
    for racer in racers {
        let outcome = tokio::time::timeout(NO_DEADLOCK, racer)
            .await
            .expect("no deadlock")
            .expect("join");
        let token = token_of(&outcome).unwrap_or_else(|| panic!("not acquired: {outcome:?}"));
        assert_eq!(h.holds(token).await, 2, "a hold on every Quota of the plan");
    }
    // Every Quota sits in two of the three sets: 8 of the 12 acquisitions.
    for (holder, id) in ["u1", "u2", "u3"].into_iter().zip(ids) {
        assert_eq!(h.consumed(holder, id).await, 8, "{holder}");
    }
    h.down().await;
}

/// An evaluator that stops inside the acquisition's transaction, past its
/// scope lock and record recheck, until the test lets it go.
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

/// Commit a record under the scope `holders` acquire with, without the scope
/// lock: the writer an acquisition can still lose the record's key to.
async fn commit_record_out_of_band(
    h: &PgHarness,
    holders: &[&str],
    key: &str,
    payload: u8,
    blob: String,
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
            subject_key: Set(IdempotencySubjectKey::of(&subjects(holders))
                .as_bytes()
                .to_vec()),
            operation_type: Set(OperationType::Reserve.as_str().to_owned()),
            idem_key: Set(key.to_owned()),
            payload_hash: Set(vec![payload; 32]),
            decision_blob: Set(blob),
            applied_entries: Set(None),
            attribution_hash: Set(Some(vec![7; 32])),
            reversed_by_key: Set(None),
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
        .expect("commit the competing record");
}

fn winners_lease() -> EvaluatedLease {
    EvaluatedLease {
        decision: Decision::allowed_with_plan(std::collections::BTreeMap::new()),
        token: Some(LeaseToken::new(Uuid::from_u128(0x0011_7e55))),
        expires_at: None,
    }
}

/// Start an acquisition of 5 on `u1` under `key` that stops in its evaluation.
fn hold_an_acquisition(
    h: &PgHarness,
    key: &str,
) -> (
    tokio::task::JoinHandle<Acquired>,
    std::sync::mpsc::SyncSender<()>,
) {
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let holder = h.acquire_with(&["u1"], 5, key, held_evaluator(locked_tx, release_rx));
    locked_rx
        .recv_timeout(NO_DEADLOCK)
        .expect("the acquisition is evaluating");
    (holder, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acquisition_that_loses_the_record_key_replays_the_winner_and_holds_nothing() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let (holder, release) = hold_an_acquisition(&h, "raced");
    let blob = serde_json::to_string(&winners_lease()).expect("lease");
    // The acquisition payload is its amount, 5.
    commit_record_out_of_band(&h, &["u1"], "raced", 5, blob).await;
    release.send(()).expect("release");

    match holder.await.expect("join") {
        Ok(TransitionOutcome::NoOp(replayed)) => {
            assert_eq!(replayed, winners_lease(), "the winner's lease");
        }
        other => panic!("expected a replay of the winner, got {other:?}"),
    }
    assert_eq!(
        h.consumed("u1", id).await,
        0,
        "the loser's hold rolled back"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acquisition_that_loses_the_record_key_to_another_payload_is_a_mismatch() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let (holder, release) = hold_an_acquisition(&h, "raced");
    let blob = serde_json::to_string(&winners_lease()).expect("lease");
    commit_record_out_of_band(&h, &["u1"], "raced", 6, blob).await;
    release.send(()).expect("release");

    assert!(
        matches!(
            holder.await.expect("join"),
            Err(StorageError::IdempotencyPayloadMismatch)
        ),
        "the same key under another payload is the caller's conflict"
    );
    assert_eq!(h.consumed("u1", id).await, 0);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_backlog_beyond_one_statement_of_parameters_does_not_block_a_writer() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    // Postgres binds at most 65,535 parameters in one statement.
    h.seed_expired_leases(70_000).await;

    h.held(&["u1"], 10, "a1").await;
    assert_eq!(h.consumed("u1", id).await, 10);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_hold_check_survives_a_backlog_beyond_one_statement_of_parameters() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    let period = Uuid::now_v7();
    h.seed_unreturned_holds(id, Some(period), 70_000).await;

    let conn = h.db.conn().expect("conn");
    let live = lease_repo::has_live_holds(&conn, &scope(), period, h.now())
        .await
        .expect("the check runs");
    assert!(!live, "every seeded lease has expired");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_deactivation_lookup_survives_a_backlog_beyond_one_statement_of_parameters() {
    let h = PgHarness::up().await;
    let id = h.quota("u1", Some(100)).await;
    h.seed_unreturned_holds(id, None, 70_000).await;

    let conn = h.db.conn().expect("conn");
    let holders = lease_repo::active_on_quota_for_update(
        &conn,
        &scope(),
        id.as_uuid(),
        h.now(),
        lease_repo::RowWait::Wait,
    )
    .await
    .expect("the lookup runs");
    assert!(holders.is_empty(), "every seeded lease has expired");
    h.down().await;
}
