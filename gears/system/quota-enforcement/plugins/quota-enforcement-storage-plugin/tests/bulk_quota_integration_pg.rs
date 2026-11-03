#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed suite of the bulk Quota envelopes.
//!
//! What only a real concurrent backend can show:
//!
//! - envelopes that lock overlapping Quota sets, listed in opposite orders,
//!   never deadlock and never apply in part;
//! - a bulk cap reduction racing debits never leaves a Quota consumed past its
//!   committed cap: the guard is decided under the row lock;
//! - two identical envelopes under one key apply once: the other replays the
//!   same outcome, or is refused on the stripe and replays when retried;
//! - an envelope racing a single-Quota deactivation over shared leases queues
//!   on the other's locks instead of closing a cycle with them.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test bulk_quota_integration_pg`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, BulkCreateEntry, BulkCreateEnvelope, BulkDeactivateEntry,
    BulkDeactivateEnvelope, BulkDeactivated, BulkUpdateEntry, BulkUpdateEnvelope, BulkUpdated,
    CapPatch, DeactivateOutcome, Decision, EvaluatedMutation, IdempotencyScope,
    IdempotencySubjectKey, IdempotencyWrite, LeaseToken, MetricId, OperationType, PageRequest,
    PayloadHash, PeriodType, PolicyDraft, PolicyScope, Quota, QuotaDebitPlan, QuotaDraft,
    QuotaFilter, QuotaId, QuotaPatch, QuotaType, StorageError, SubjectRef, TenantId,
    TransitionOutcome,
};
use sea_orm::{ColumnTrait as _, EntityTrait as _, QueryFilter as _, QuerySelect as _};
use sea_orm_migration::MigratorTrait as _;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::secure::SecureEntityExt as _;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use quota_enforcement_storage_plugin::domain::ports::LeaseStore as _;
use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::infra::storage::entity::lease_hold;
use quota_enforcement_storage_plugin::infra::storage::repo::{
    RowWait, consumption_counter_repo, quota_repo,
};
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationOutbox, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, StoreError, start_undelivered_outbox,
};

mod common;
use common::lock_waiters;

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

fn consumption(holder: &str, cap: u64) -> QuotaDraft {
    let mut draft = quota_draft(subject(holder), Some(cap));
    draft.tenant_id = tenant();
    draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    draft.quota_type = QuotaType::Consumption;
    draft.period = Some(PeriodType::Day);
    draft
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

fn cap_update(key: &str, ids: &[QuotaId], cap: u64) -> BulkUpdateEnvelope {
    BulkUpdateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkUpdateQuotas, key, 1),
        items: ids
            .iter()
            .map(|quota_id| BulkUpdateEntry {
                idempotency_key: None,
                scope: scope(),
                quota_id: *quota_id,
                patch: QuotaPatch {
                    cap: Some(CapPatch::Bounded(cap)),
                    ..QuotaPatch::default()
                },
                events: Vec::new(),
            })
            .collect(),
    }
}

fn deactivation(key: &str, ids: &[QuotaId]) -> BulkDeactivateEnvelope {
    BulkDeactivateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkDeactivateQuotas, key, 1),
        items: ids
            .iter()
            .map(|quota_id| BulkDeactivateEntry {
                idempotency_key: None,
                scope: scope(),
                quota_id: *quota_id,
                events: Vec::new(),
            })
            .collect(),
    }
}

fn applicable(holders: &[&str]) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects: holders.iter().map(|holder| subject(holder)).collect(),
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

fn mutation_write(applicable: &ApplicableQuotas, op: OperationType, key: &str) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&applicable.subjects),
            operation_type: op,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([1; 32]),
    }
}

/// `amount` over `applicable`, allowed where it fits.
fn mutation<'a>(
    applicable: &'a ApplicableQuotas,
    idempotency: &'a IdempotencyWrite,
    null: &'a serde_json::Value,
    amount: u64,
) -> EvaluatedMutation<'a> {
    EvaluatedMutation {
        applicable,
        amount,
        request: null,
        resource: null,
        user_projection: None,
        limits: limits(),
        idempotency,
        authorized: AttributionDigest::from_bytes([7; 32]),
        evaluate: allow_within_cap(),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(500).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Allow the requested amount against every applicable Quota with room for
/// it; deny otherwise.
fn allow_within_cap() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        let fits = context.quotas.iter().all(|quota| {
            quota
                .snapshot
                .remaining
                .is_none_or(|remaining| remaining >= context.amount)
        });
        let decision = if fits {
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
                result: quota_enforcement_sdk::DecisionResult::Denied {
                    violated_quota_ids: context
                        .quotas
                        .iter()
                        .map(|quota| quota.snapshot.quota_id)
                        .collect(),
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

/// A row a holder transaction keeps locked.
enum Held {
    /// The idempotency stripe of a scope.
    Stripe(i32),
    Quota(QuotaId),
    /// The latest consumption counter row of a Quota.
    Counter(QuotaId),
    Hold {
        lease: LeaseToken,
        quota: QuotaId,
    },
}

impl Held {
    fn stripe(scope: &IdempotencyScope) -> Self {
        use quota_enforcement_storage_plugin::infra::storage::repo::idempotency_repo::{
            ScopeKey, stripe_of,
        };
        Self::Stripe(stripe_of(&ScopeKey {
            tenant_id: scope.tenant_id.as_uuid(),
            subject_key: scope.subject_key.as_bytes(),
            operation_type: scope.operation_type.as_str(),
            idem_key: &scope.key,
        }))
    }
}

struct PgHarness {
    db: toolkit_db::Db,
    quotas: Arc<SqlQuotaStore>,
    debits: Arc<SqlConsumptionStore>,
    outbox: OutboxHandle,
    /// The quota store's clock: wall time unless pinned.
    pinned: Arc<Mutex<Option<OffsetDateTime>>>,
    container: ContainerAsync<Postgres>,
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
        let outbox = start_undelivered_outbox(db.clone()).await.expect("outbox");
        let bound = Arc::new(QeOutbox::new());
        bound.bind(Arc::clone(outbox.outbox())).expect("bind once");
        let enqueuer: Arc<dyn NotificationOutbox> = bound;
        SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer))
            .create_policy(
                &ctx(),
                PolicyDraft {
                    scope: PolicyScope::Global,
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
        let pinned = Arc::new(Mutex::new(None));
        let reader = Arc::clone(&pinned);
        let quotas = Arc::new(SqlQuotaStore::with_clock(
            db.clone(),
            Arc::clone(&enqueuer),
            Arc::new(move || {
                reader
                    .lock()
                    .expect("clock")
                    .unwrap_or_else(OffsetDateTime::now_utc)
            }),
        ));
        let debits = Arc::new(SqlConsumptionStore::new(db.clone(), enqueuer));
        Self {
            db,
            quotas,
            debits,
            outbox,
            pinned,
            container,
        }
    }

    fn set_now(&self, now: OffsetDateTime) {
        *self.pinned.lock().expect("clock") = Some(now);
    }

    /// Wait until `count` sessions queue on a lock.
    async fn until_lock_waiters(&self, count: u32) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while lock_waiters(&self.container).await < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{count} sessions never queued on a lock"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    /// Give `metric` a contention budget (I8); without one the platform
    /// default applies: 0 ms, fail fast.
    async fn set_contention_timeout(&self, metric: &str, timeout: Duration) {
        use quota_enforcement_storage_plugin::infra::storage::entity::contention_timeout_config;
        use sea_orm::ActiveValue::Set;
        let conn = self.db.conn().expect("conn");
        toolkit_db::secure::secure_insert::<contention_timeout_config::Entity>(
            contention_timeout_config::ActiveModel {
                metric_key: Set(metric.to_owned()),
                timeout_ms: Set(i64::try_from(timeout.as_millis()).expect("fits")),
                updated_at: Set(time::OffsetDateTime::now_utc()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await
        .expect("configure the contention budget");
    }

    /// Hold `held` in a transaction of its own until the returned sender fires.
    async fn hold(
        &self,
        held: Held,
    ) -> (
        tokio::task::JoinHandle<Result<(), HolderError>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use quota_enforcement_storage_plugin::infra::storage::repo::idempotency_repo::lock_stripe;
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let db = self.db.clone();
        let holder = tokio::spawn(async move {
            db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let all = AccessScope::allow_all();
                    match held {
                        Held::Stripe(stripe) => {
                            assert!(lock_stripe(tx, stripe).await?, "the stripe exists");
                        }
                        Held::Quota(id) => {
                            quota_repo::find_by_id(tx, &all, id.as_uuid(), Some(RowWait::Wait))
                                .await?
                                .expect("the Quota row");
                        }
                        Held::Counter(id) => {
                            consumption_counter_repo::find_latest_for_update(
                                tx,
                                &all,
                                id.as_uuid(),
                                RowWait::Wait,
                            )
                            .await?
                            .expect("the counter row");
                        }
                        Held::Hold { lease, quota } => {
                            lease_hold::Entity::find()
                                .filter(lease_hold::Column::LeaseToken.eq(lease.as_uuid()))
                                .filter(lease_hold::Column::QuotaId.eq(quota.as_uuid()))
                                .lock(sea_orm::sea_query::LockType::Update)
                                .secure()
                                .scope_with(&all)
                                .one(tx)
                                .await?
                                .expect("the hold row");
                        }
                    }
                    held_tx.send(()).expect("announce");
                    release_rx.await.expect("released");
                    Ok::<(), HolderError>(())
                })
            })
            .await
        });
        held_rx.await.expect("the row is held");
        (holder, release_tx)
    }

    /// A ten-minute lease of 1 over every Quota of `holders`.
    async fn lease(&self, holders: &[&str], key: &str) -> LeaseToken {
        let applicable = applicable(holders);
        let idempotency = mutation_write(&applicable, OperationType::Reserve, key);
        let null = serde_json::Value::Null;
        self.debits
            .acquire_lease(
                &ctx(),
                &scope(),
                &mutation(&applicable, &idempotency, &null, 1),
                Duration::from_mins(10),
            )
            .await
            .expect("acquire")
            .into_inner()
            .token
            .expect("a lease was issued")
    }

    fn spawn_deactivate(
        &self,
        id: QuotaId,
    ) -> tokio::task::JoinHandle<Result<DeactivateOutcome, StoreError>> {
        let quotas = Arc::clone(&self.quotas);
        tokio::spawn(async move { quotas.deactivate_quota(&actor(), &scope(), id, &[]).await })
    }

    fn spawn_bulk_deactivate(
        &self,
        envelope: BulkDeactivateEnvelope,
    ) -> tokio::task::JoinHandle<Result<TransitionOutcome<BulkDeactivated>, StoreError>> {
        let quotas = Arc::clone(&self.quotas);
        tokio::spawn(async move { quotas.bulk_deactivate_quotas(&actor(), &envelope).await })
    }

    fn spawn_bulk_update(
        &self,
        envelope: BulkUpdateEnvelope,
    ) -> tokio::task::JoinHandle<Result<TransitionOutcome<BulkUpdated>, StoreError>> {
        let quotas = Arc::clone(&self.quotas);
        tokio::spawn(async move { quotas.bulk_update_quotas(&actor(), &envelope).await })
    }

    async fn quota(&self, holder: &str, cap: u64) -> QuotaId {
        self.quotas
            .create_quota(&actor(), &scope(), consumption(holder, cap), &[])
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

    async fn consumed(&self, holder: &str, id: QuotaId) -> u64 {
        self.debits
            .read_quota_snapshot(
                &ctx(),
                &scope(),
                &ApplicableQuotas {
                    tenant_id: tenant(),
                    subjects: vec![subject(holder)],
                    metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
                },
            )
            .await
            .expect("snapshot")
            .into_iter()
            .find(|snapshot| snapshot.quota_id == id)
            .map_or(0, |snapshot| snapshot.consumed)
    }
}

async fn debit(store: &SqlConsumptionStore, holder: &str, amount: u64, key: &str) {
    let applicable = applicable(&[holder]);
    let idempotency = mutation_write(&applicable, OperationType::Debit, key);
    let null = serde_json::Value::Null;
    let outcome = store
        .apply_debit_plan(
            &ctx(),
            &scope(),
            &mutation(&applicable, &idempotency, &null, amount),
            &[],
        )
        .await;
    // Allowed, denied, or refused on a held row: every one is a legitimate
    // outcome of the race.
    assert!(
        matches!(outcome, Ok(_) | Err(StorageError::LeaseContentionTimeout)),
        "{outcome:?}"
    );
}

/// Let the holder commit, then await both racers; a deadlock aborts one of them.
async fn release_and_join<A, B>(
    holder: tokio::task::JoinHandle<Result<(), HolderError>>,
    release: tokio::sync::oneshot::Sender<()>,
    one: tokio::task::JoinHandle<A>,
    two: tokio::task::JoinHandle<B>,
) -> (A, B) {
    release.send(()).expect("release");
    holder.await.expect("join").expect("holder commits");
    let (one, two) =
        tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(one, two) })
            .await
            .expect("both finish");
    (one.expect("join"), two.expect("join"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn envelopes_over_overlapping_quotas_in_opposite_orders_never_deadlock() {
    let h = PgHarness::up().await;
    let mut ids = Vec::new();
    for holder in ["a", "b", "c"] {
        ids.push(h.quota(holder, 100).await);
    }
    let reversed: Vec<QuotaId> = ids.iter().rev().copied().collect();
    let rounds = 10;
    for round in 0..rounds {
        let forward = cap_update(&format!("f{round}"), &ids, 200 + round);
        let backward = cap_update(&format!("b{round}"), &reversed, 300 + round);
        let who = actor();
        let (one, two) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                h.quotas.bulk_update_quotas(&who, &forward),
                h.quotas.bulk_update_quotas(&who, &backward),
            )
        })
        .await
        .expect("no deadlock");
        assert!(one.is_ok(), "{one:?}");
        assert!(two.is_ok(), "{two:?}");
    }
    let mut versions = Vec::new();
    for id in &ids {
        versions.push(h.get(*id).await.record_version);
    }
    let applied = u32::try_from(rounds * 2).expect("small") + 1;
    assert_eq!(
        versions,
        vec![applied; 3],
        "every envelope applied to every Quota, none in part"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bulk_cap_reduction_racing_debits_never_leaves_a_quota_over_its_cap() {
    let h = PgHarness::up().await;
    let raced = h.quota("hot", 100).await;
    let other = h.quota("cold", 100).await;
    let debits: Vec<_> = (0..20)
        .map(|n| {
            let store = Arc::clone(&h.debits);
            tokio::spawn(async move { debit(&store, "hot", 5, &format!("d{n}")).await })
        })
        .collect();
    let shrink = cap_update("shrink", &[other, raced], 40);
    let update = h.quotas.bulk_update_quotas(&actor(), &shrink).await;
    for task in debits {
        task.await.expect("debit task");
    }
    let cap = h.get(raced).await.cap.expect("bounded");
    let consumed = h.consumed("hot", raced).await;
    assert!(consumed <= cap, "consumed {consumed} past cap {cap}");
    match update {
        Ok(_) => assert_eq!(h.get(other).await.cap, Some(40)),
        Err(StoreError::BulkItem { index, cause }) => {
            assert_eq!(index, 1, "only the raced Quota can trip the guard");
            assert!(
                matches!(*cause, StoreError::CapBelowConsumed { .. }),
                "{cause:?}"
            );
            assert_eq!(h.get(other).await.cap, Some(100), "nothing applied");
        }
        Err(other) => panic!("unexpected {other:?}"),
    }
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_envelopes_under_one_key_apply_once() {
    let h = PgHarness::up().await;
    let envelope = BulkCreateEnvelope {
        tenant_id: tenant(),
        idempotency: envelope_write(OperationType::BulkCreateQuotas, "pack", 1),
        items: vec![BulkCreateEntry {
            idempotency_key: None,
            scope: scope(),
            draft: consumption("u1", 10),
            events: Vec::new(),
        }],
    };
    let who = actor();
    let (one, two) = tokio::join!(
        h.quotas.bulk_create_quotas(&who, &envelope),
        h.quotas.bulk_create_quotas(&who, &envelope),
    );
    let settle = |outcome: Result<TransitionOutcome<_>, StoreError>| async {
        match outcome {
            Ok(outcome) => outcome,
            // Refused on the stripe at the 0 ms default: a retry replays.
            Err(StoreError::ContentionTimeout) => h
                .quotas
                .bulk_create_quotas(&actor(), &envelope)
                .await
                .expect("the retry replays"),
            Err(other) => panic!("unexpected {other:?}"),
        }
    };
    let one = settle(one).await;
    let two = settle(two).await;
    assert_eq!(
        [one.is_applied(), two.is_applied()]
            .iter()
            .filter(|applied| **applied)
            .count(),
        1,
        "exactly one application"
    );
    assert_eq!(one.get(), two.get(), "both answer the same outcome");
    let page = h
        .quotas
        .read_quotas(
            &AccessScope::allow_all(),
            QuotaFilter::default(),
            PageRequest::first(10),
        )
        .await
        .expect("read");
    assert_eq!(page.items.len(), 1, "one Quota created");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_stripe_is_waited_on_within_the_budget_of_the_targets_metric() {
    let h = PgHarness::up().await;
    let id = h.quota("a", 100).await;
    let envelope = cap_update("wait", &[id], 200);
    let who = actor();

    let (holder, release) = h.hold(Held::stripe(&envelope.idempotency.scope)).await;
    assert_eq!(
        h.quotas.bulk_update_quotas(&who, &envelope).await,
        Err(StoreError::ContentionTimeout),
        "at the 0 ms default a held stripe is refused at once"
    );

    // The target's metric now allows a wait: the envelope waits for the
    // holder, then applies.
    h.set_contention_timeout(METRIC_TOKENS, Duration::from_secs(10))
        .await;
    let quotas = Arc::clone(&h.quotas);
    let waiting = {
        let envelope = envelope.clone();
        tokio::spawn(async move { quotas.bulk_update_quotas(&actor(), &envelope).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "it waits on the held stripe");
    release.send(()).expect("release");
    holder.await.expect("join").expect("holder commits");
    let outcome = waiting
        .await
        .expect("join")
        .expect("applied after the wait");
    assert!(outcome.is_applied());
    assert_eq!(h.get(id).await.cap, Some(200));
    h.down().await;
}

/// Why the stripe holder's transaction failed; read through `Debug` when it does.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the fields are read through Debug in a failed expect"
)]
enum HolderError {
    Db(toolkit_db::DbError),
    Scope(toolkit_db::secure::ScopeError),
}

impl From<toolkit_db::DbError> for HolderError {
    fn from(error: toolkit_db::DbError) -> Self {
        Self::Db(error)
    }
}

impl From<toolkit_db::secure::ScopeError> for HolderError {
    fn from(error: toolkit_db::secure::ScopeError) -> Self {
        Self::Scope(error)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bulk_deactivate_and_a_deactivation_over_a_shared_quota_queue_instead_of_deadlocking() {
    let h = PgHarness::up().await;
    let first = h.quota("u1", 100).await;
    let second = h.quota("u2", 100).await;
    let shared = h.quota("t", 100).await;
    let one = h.lease(&["u1", "t"], "l1").await;
    let two = h.lease(&["u2", "t"], "l2").await;
    // The envelope stops on the shared counter, past its leases and capacity row.
    let (holder, release) = h.hold(Held::Counter(shared)).await;
    let bulk = h.spawn_bulk_deactivate(deactivation("off", &[first, shared]));
    h.until_lock_waiters(1).await;
    // It queues on the capacity row, or on the second lease once the envelope locks every lease first.
    let single = h.spawn_deactivate(second);
    h.until_lock_waiters(2).await;
    let (bulk, single) = release_and_join(holder, release, bulk, single).await;
    let bulk = bulk.expect("the envelope commits").into_inner();
    assert_eq!(bulk.items[0].resolved_leases, vec![one]);
    assert_eq!(bulk.items[1].resolved_leases, vec![two]);
    let single = single.expect("the deactivation commits");
    assert_eq!(
        single.resolved_leases,
        Vec::new(),
        "the envelope resolved it"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bulk_update_listing_its_quotas_out_of_order_queues_behind_a_deactivation() {
    let h = PgHarness::up().await;
    let gone = h.quota("x", 100).await;
    let a = h.quota("a", 100).await;
    let b = h.quota("b", 100).await;
    let (low, high) = if a.as_uuid() < b.as_uuid() {
        (a, b)
    } else {
        (b, a)
    };
    h.lease(&["x", "a", "b"], "l").await;
    // The deactivation stops on the low counter.
    let (holder, release) = h.hold(Held::Counter(low)).await;
    let single = h.spawn_deactivate(gone);
    h.until_lock_waiters(1).await;
    // It takes the high counter before queuing on the low one, or queues on the low one first.
    let bulk = h.spawn_bulk_update(cap_update("raise", &[high, low], 200));
    h.until_lock_waiters(2).await;
    let (single, bulk) = release_and_join(holder, release, single, bulk).await;
    single.expect("the deactivation commits");
    assert!(bulk.expect("the envelope commits").is_applied());
    for id in [low, high] {
        assert_eq!(h.get(id).await.cap, Some(200));
    }
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deactivation_locks_the_counters_of_all_its_leases_in_one_order() {
    let h = PgHarness::up().await;
    let gone = h.quota("x", 100).await;
    let low = h.quota("p", 100).await;
    let high = h.quota("q", 100).await;
    // Tokens are time-ordered: the lease on the high Quota, taken first, is resolved first.
    let first_lease = h.lease(&["x", "q"], "on-high").await;
    let second_lease = h.lease(&["x", "p"], "on-low").await;
    assert!(low.as_uuid() < high.as_uuid());
    assert!(first_lease.as_uuid() < second_lease.as_uuid());
    // The deactivation stops on the first lease's high hold, the high counter locked.
    let (holder, release) = h
        .hold(Held::Hold {
            lease: first_lease,
            quota: high,
        })
        .await;
    let single = h.spawn_deactivate(gone);
    h.until_lock_waiters(1).await;
    // It takes the low counter before queuing on the high one, or queues on the low one first.
    let bulk = h.spawn_bulk_update(cap_update("raise", &[low, high], 200));
    h.until_lock_waiters(2).await;
    let (single, bulk) = release_and_join(holder, release, single, bulk).await;
    let single = single.expect("the deactivation commits");
    assert_eq!(single.resolved_leases.len(), 2);
    assert!(bulk.expect("the envelope commits").is_applied());
    h.down().await;
}

/// A bulk deactivate that waits on `held` while its Quota's ten-minute lease runs out.
async fn deactivate_while_the_lease_runs_out(held: fn(QuotaId) -> Held) {
    let h = PgHarness::up().await;
    let id = h.quota("u1", 100).await;
    h.lease(&["u1"], "l").await;
    let (holder, release) = h.hold(held(id)).await;
    let bulk = h.spawn_bulk_deactivate(deactivation("off", &[id]));
    h.until_lock_waiters(1).await;
    h.set_now(OffsetDateTime::now_utc() + time::Duration::minutes(11));
    release.send(()).expect("release");
    holder.await.expect("join").expect("holder commits");
    let outcome = bulk
        .await
        .expect("join")
        .expect("the envelope commits")
        .into_inner();
    assert_eq!(
        outcome.items[0].resolved_leases,
        Vec::new(),
        "an expired lease is the sweeper's"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_that_expires_while_a_bulk_deactivate_waits_on_its_quota_is_left_to_the_sweeper() {
    deactivate_while_the_lease_runs_out(Held::Quota).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_that_expires_while_a_bulk_deactivate_waits_on_a_counter_is_left_to_the_sweeper() {
    deactivate_while_the_lease_runs_out(Held::Counter).await;
}
