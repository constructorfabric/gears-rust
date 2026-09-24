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
//!   plan or on none, and never deadlock (ADR-0002).
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test lease_store_integration_pg`.

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
use quota_enforcement_storage_plugin::infra::storage::repo::lease_repo;
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
    /// The one clock of both stores, so a test can move every lease past its
    /// expiry at once.
    clock: Arc<Mutex<OffsetDateTime>>,
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
            clock,
            outbox,
            _container: container,
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
