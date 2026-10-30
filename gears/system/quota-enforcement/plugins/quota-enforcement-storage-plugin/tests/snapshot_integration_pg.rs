#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed suite of the snapshot reads.
//!
//! What only a real concurrent backend can show:
//!
//! - a page is read from one snapshot: writers committing between its page
//!   query and its assembly change neither a counter nor a Quota row on it;
//! - a period row a writer commits after the read's snapshot is neither seen
//!   nor duplicated: the read reports zero and the writer's row stands alone;
//! - under a tenant-subtree grant, compiled against `tenant_closure` by the
//!   secure layer, both reads return the visible Quota and not the other;
//! - a read writes nothing (I3): across a boundary it opens, settles and
//!   emits nothing, and the debit that crosses it emits the one
//!   `period-rollover`;
//! - a read takes no row lock: it finishes while a debit holds the Quota row,
//!   and a debit is not refused while a read is in flight;
//! - a page issues as many statements for twenty Quotas as for two.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test snapshot_integration_pg`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, PolicySchemaSnapshot, TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, CapPatch, EvaluatedMutation, IdempotencyScope,
    IdempotencySubjectKey, IdempotencyWrite, MetricId, OperationType, PageRequest, PageResult,
    PayloadHash, PeriodType, PolicyDraft, PolicyScope, QuotaDraft, QuotaId, QuotaPatch,
    QuotaSnapshot, QuotaType, StorageError, SubjectRef, TenantId,
};
use sea_orm::EntityTrait as _;
use sea_orm_migration::prelude::*;
use testcontainers::core::ExecCommand;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::infra::storage::entity::quota_consumption_counter;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NotificationOutbox, QeOutbox, QuotaStore, SqlConsumptionStore,
    SqlPolicyStore, SqlQuotaStore, start_undelivered_outbox,
};

mod common;

use common::{allow_all, limits};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1";
/// A Tuesday, inside an ordinary day period.
const DAY_ONE: OffsetDateTime = datetime!(2026-03-17 10:00:00 UTC);
const DAY_TWO: OffsetDateTime = datetime!(2026-03-18 10:00:00 UTC);

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

fn applicable(tenant: TenantId, holder: &str) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant,
        subjects: vec![subject(holder)],
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

fn allocation(tenant: TenantId, holder: &str, cap: u64) -> QuotaDraft {
    let mut draft = quota_draft(subject(holder), Some(cap));
    draft.tenant_id = tenant;
    draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    draft
}

fn consumption(tenant: TenantId, holder: &str, cap: u64) -> QuotaDraft {
    QuotaDraft {
        quota_type: QuotaType::Consumption,
        period: Some(PeriodType::Day),
        ..allocation(tenant, holder, cap)
    }
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

/// Probe over the outbox body table: what was enqueued.
mod outbox_body {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "qe_outbox_body")]
    #[secure(no_tenant, no_resource, no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub payload: Vec<u8>,
        pub payload_type: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl sea_orm::ActiveModelBehavior for ActiveModel {}
}

/// A test-only `tenant_closure`, the Account Management table a
/// tenant-subtree filter compiles against; QE owns no migration for it.
struct TenantClosure {
    edges: Vec<(Uuid, Uuid)>,
}

impl MigrationName for TenantClosure {
    fn name(&self) -> &'static str {
        "m_test_tenant_closure"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TenantClosure {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("tenant_closure"))
                    .col(ColumnDef::new(Alias::new("ancestor_id")).uuid().not_null())
                    .col(
                        ColumnDef::new(Alias::new("descendant_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("barrier"))
                            .small_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("descendant_status"))
                            .small_integer()
                            .not_null()
                            .default(1),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("ancestor_id"))
                            .col(Alias::new("descendant_id")),
                    )
                    .to_owned(),
            )
            .await?;
        for (ancestor, descendant) in &self.edges {
            manager
                .exec_stmt(
                    Query::insert()
                        .into_table(Alias::new("tenant_closure"))
                        .columns([Alias::new("ancestor_id"), Alias::new("descendant_id")])
                        .values_panic([(*ancestor).into(), (*descendant).into()])
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

/// Holds a reader's first clock reading — taken after its page query, so its
/// snapshot is already fixed — until the test lets it go.
struct Gate {
    armed: AtomicBool,
    reached: tokio::sync::mpsc::UnboundedSender<()>,
    go: Mutex<std::sync::mpsc::Receiver<()>>,
}

struct GateHandle {
    reached: tokio::sync::mpsc::UnboundedReceiver<()>,
    go: std::sync::mpsc::Sender<()>,
}

impl GateHandle {
    async fn wait_until_reached(&mut self) {
        tokio::time::timeout(Duration::from_secs(20), self.reached.recv())
            .await
            .expect("the reader reached its clock")
            .expect("gate open");
    }

    fn release(&self) {
        self.go.send(()).expect("release the reader");
    }
}

struct PgHarness {
    db: toolkit_db::Db,
    store: Arc<SqlConsumptionStore>,
    quotas: SqlQuotaStore,
    enqueuer: Arc<dyn NotificationOutbox>,
    clock: Arc<Mutex<OffsetDateTime>>,
    outbox: OutboxHandle,
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
        let clock = Arc::new(Mutex::new(DAY_ONE));
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
            enqueuer,
            clock,
            outbox,
            container,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    fn set_now(&self, now: OffsetDateTime) {
        *self.clock.lock().expect("clock") = now;
    }

    /// A second store over the same database whose first clock reading waits
    /// on the returned gate.
    fn gated_reader(&self) -> (Arc<SqlConsumptionStore>, GateHandle) {
        let (reached_tx, reached_rx) = tokio::sync::mpsc::unbounded_channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let gate = Arc::new(Gate {
            armed: AtomicBool::new(true),
            reached: reached_tx,
            go: Mutex::new(go_rx),
        });
        let clock = Arc::clone(&self.clock);
        let reader = SqlConsumptionStore::with_clock(
            self.db.clone(),
            Arc::clone(&self.enqueuer),
            Arc::new(move || {
                if gate.armed.swap(false, Ordering::SeqCst) {
                    gate.reached.send(()).expect("announce the gate");
                    // Hand this worker's queued tasks to another thread while
                    // waiting, or a task parked in its LIFO slot would starve.
                    tokio::task::block_in_place(|| {
                        gate.go.lock().expect("gate").recv().expect("released");
                    });
                }
                *clock.lock().expect("clock")
            }),
        );
        (
            Arc::new(reader),
            GateHandle {
                reached: reached_rx,
                go: go_tx,
            },
        )
    }

    async fn create(&self, draft: QuotaDraft) -> QuotaId {
        let tenant = draft.tenant_id;
        self.quotas
            .create_quota(
                &actor(),
                &AccessScope::for_tenant(tenant.as_uuid()),
                draft,
                &[],
            )
            .await
            .expect("quota")
    }

    async fn debit(&self, holder: &str, amount: u64, key: &str) {
        debit_with(Arc::clone(&self.store), holder, amount, key, allow_all())
            .await
            .expect("debit");
    }

    /// Sessions `PostgreSQL` shows waiting on a lock.
    async fn lock_waiters(&self) -> u32 {
        let sql = "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'";
        let args = ["psql", "-tA", "-U", "user", "-d", "app", "-c", sql];
        let mut output = self
            .container
            .exec(ExecCommand::new(args.map(str::to_owned)))
            .await
            .expect("psql runs in the container");
        let stdout = output.stdout_to_vec().await.expect("psql output");
        String::from_utf8_lossy(&stdout)
            .trim()
            .parse()
            .expect("a count")
    }

    async fn periods(&self, id: QuotaId) -> Vec<quota_consumption_counter::Model> {
        use sea_orm::ColumnTrait;
        let conn = self.db.conn().expect("connection");
        quota_consumption_counter::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all().add(quota_consumption_counter::Column::QuotaId.eq(id.as_uuid())),
            )
            .all(&conn)
            .await
            .expect("period rows")
    }

    async fn rollovers(&self) -> usize {
        let conn = self.db.conn().expect("connection");
        outbox_body::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .expect("outbox")
            .into_iter()
            .filter(|message| message.payload_type == "period-rollover")
            .count()
    }
}

/// Debit `amount` from `holder`'s Quotas through `evaluate`.
async fn debit_with(
    store: Arc<SqlConsumptionStore>,
    holder: &str,
    amount: u64,
    key: &str,
    evaluate: Arc<TransactionEvaluator>,
) -> Result<(), StorageError> {
    let applicable = applicable(tenant(), holder);
    let idempotency = IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::of(&applicable.subjects),
            operation_type: OperationType::Debit,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([1; 32]),
    };
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
        .map(|_| ())
}

/// [`allow_all`] whose first call waits on the returned gate. The debit
/// calls it inside its transaction, after it has locked the Quota rows and
/// read their counters, and before it writes.
fn gated_evaluator() -> (Arc<TransactionEvaluator>, GateHandle) {
    let (reached_tx, reached_rx) = tokio::sync::mpsc::unbounded_channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let gate = Gate {
        armed: AtomicBool::new(true),
        reached: reached_tx,
        go: Mutex::new(go_rx),
    };
    let allow = allow_all();
    let evaluate: Arc<TransactionEvaluator> = Arc::new(move |context: &EvaluationContext<'_>| {
        if gate.armed.swap(false, Ordering::SeqCst) {
            gate.reached.send(()).expect("announce the gate");
            tokio::task::block_in_place(|| {
                gate.go.lock().expect("gate").recv().expect("released");
            });
        }
        allow(context)
    });
    (
        evaluate,
        GateHandle {
            reached: reached_rx,
            go: go_tx,
        },
    )
}

/// Counts the statements `sqlx` reports while it dispatches for one future.
#[derive(Clone, Default)]
struct Statements(Arc<AtomicUsize>);

impl Statements {
    /// `op`'s output and the statements it issued.
    async fn during<T>(op: impl std::future::Future<Output = T>) -> (T, usize) {
        use tracing::instrument::WithSubscriber as _;
        let counter = Self::default();
        let out = op
            .with_subscriber(tracing::Dispatch::new(counter.clone()))
            .await;
        (out, counter.0.load(Ordering::SeqCst))
    }
}

impl tracing::Subscriber for Statements {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "sqlx::query"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn of(page: &PageResult<QuotaSnapshot>, id: QuotaId) -> &QuotaSnapshot {
    page.items
        .iter()
        .find(|snapshot| snapshot.quota_id == id)
        .expect("snapshot on the page")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_reads_one_snapshot_while_writers_commit_around_it() {
    let h = PgHarness::up().await;
    let counted = h.create(consumption(tenant(), "u1", 100)).await;
    let capped = h.create(allocation(tenant(), "u1", 50)).await;
    h.debit("u1", 10, "before").await;
    let (reader, mut gate) = h.gated_reader();

    let read = tokio::spawn(async move {
        reader
            .bulk_read_quota_snapshot(
                &ctx(),
                &scope(),
                &[applicable(tenant(), "u1")],
                PageRequest::first(10),
            )
            .await
    });
    gate.wait_until_reached().await;
    // The page query has run; both writers commit before assembly.
    h.debit("u1", 5, "during").await;
    h.quotas
        .update_quota(
            &actor(),
            &scope(),
            capped,
            QuotaPatch {
                cap: Some(CapPatch::Bounded(70)),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("raise the cap");
    gate.release();
    let page = read.await.expect("join").expect("page");

    assert_eq!(
        of(&page, counted).consumed,
        10,
        "the counter as of the snapshot"
    );
    assert_eq!(
        of(&page, capped).cap,
        Some(50),
        "the Quota row as of the snapshot"
    );
    let fresh = h
        .store
        .bulk_read_quota_snapshot(
            &ctx(),
            &scope(),
            &[applicable(tenant(), "u1")],
            PageRequest::first(10),
        )
        .await
        .expect("fresh page");
    assert_eq!(of(&fresh, counted).consumed, 15);
    assert_eq!(of(&fresh, capped).cap, Some(70));
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_period_row_committed_after_the_snapshot_is_neither_seen_nor_duplicated() {
    let h = PgHarness::up().await;
    let id = h.create(consumption(tenant(), "u1", 100)).await;
    let (reader, mut gate) = h.gated_reader();

    let read = tokio::spawn(async move {
        reader
            .read_quota_snapshot(&ctx(), &scope(), &applicable(tenant(), "u1"))
            .await
    });
    gate.wait_until_reached().await;
    // The reader saw no current row; a debit creates it and commits.
    h.debit("u1", 7, "first").await;
    gate.release();
    let snapshots = read.await.expect("join").expect("the read succeeds");

    assert_eq!(snapshots[0].consumed, 0, "the counter as of the snapshot");
    let rows = h.periods(id).await;
    assert_eq!(rows.len(), 1, "one row for the window");
    assert_eq!(rows[0].consumed, 7);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn under_a_subtree_grant_both_reads_return_only_what_they_see() {
    let h = PgHarness::up().await;
    let parent = tenant();
    let child = TenantId::new(Uuid::from_u128(0xc41d));
    let outside = TenantId::new(Uuid::from_u128(0x0075_de00));
    run_migrations_for_testing(
        &h.db,
        vec![Box::new(TenantClosure {
            edges: vec![
                (parent.as_uuid(), parent.as_uuid()),
                (parent.as_uuid(), child.as_uuid()),
            ],
        })],
    )
    .await
    .expect("tenant closure");
    let inside = h.create(consumption(child, "u1", 100)).await;
    let beyond = h.create(consumption(outside, "u1", 100)).await;
    let subtree = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_tenant_subtree(
        pep_properties::OWNER_TENANT_ID,
        parent.as_uuid(),
        true,
        Vec::new(),
    )]));
    let pairs = [applicable(child, "u1"), applicable(outside, "u1")];

    let page = h
        .store
        .bulk_read_quota_snapshot(&ctx(), &subtree, &pairs, PageRequest::first(10))
        .await
        .expect("bulk read under the subtree grant");
    let seen: Vec<QuotaId> = page
        .items
        .iter()
        .map(|snapshot| snapshot.quota_id)
        .collect();
    assert_eq!(seen, vec![inside]);
    let single = h
        .store
        .read_quota_snapshot(&ctx(), &subtree, &pairs[0])
        .await
        .expect("single read under the subtree grant");
    assert_eq!(single.len(), 1);
    let none = h
        .store
        .read_quota_snapshot(&ctx(), &subtree, &pairs[1])
        .await
        .expect("single read of an invisible target");
    assert!(none.is_empty());
    assert!(h.periods(inside).await.is_empty());
    assert!(h.periods(beyond).await.is_empty());
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_across_the_boundary_writes_nothing_and_the_crossing_debit_settles() {
    let h = PgHarness::up().await;
    let id = h.create(consumption(tenant(), "u1", 100)).await;
    h.debit("u1", 10, "day-one").await;
    let before = h.rollovers().await;

    h.set_now(DAY_TWO);
    let read = h
        .store
        .read_quota_snapshot(&ctx(), &scope(), &applicable(tenant(), "u1"))
        .await
        .expect("read");
    assert_eq!(read[0].consumed, 0, "day two reads as zero");
    let rows = h.periods(id).await;
    assert_eq!(rows.len(), 1, "the read opened nothing");
    assert!(!rows[0].is_settled, "and settled nothing");
    assert_eq!(h.rollovers().await, before, "and emitted nothing");

    h.debit("u1", 1, "day-two").await;
    let rows = h.periods(id).await;
    assert_eq!(rows.len(), 2, "the crossing debit opened day two");
    assert!(
        rows.iter()
            .any(|row| row.is_settled && row.period_start < DAY_TWO),
        "and settled day one"
    );
    assert_eq!(h.rollovers().await, before + 1, "exactly one rollover");
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_finishes_while_a_debit_holds_the_quota_and_writes_nothing() {
    let h = PgHarness::up().await;
    let id = h.create(consumption(tenant(), "u1", 100)).await;
    let (evaluate, mut held) = gated_evaluator();
    let debit = tokio::spawn({
        let store = Arc::clone(&h.store);
        async move { debit_with(store, "u1", 5, "held", evaluate).await }
    });
    // The debit holds the Quota row and has found no period row.
    held.wait_until_reached().await;

    let (ctx, scope, target) = (ctx(), scope(), applicable(tenant(), "u1"));
    let snapshots = {
        let read = h.store.read_quota_snapshot(&ctx, &scope, &target);
        tokio::pin!(read);
        loop {
            tokio::select! {
                done = &mut read => break done.expect("the read succeeds"),
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    assert_eq!(h.lock_waiters().await, 0, "the read waits on the debit's lock");
                }
            }
        }
    };
    assert_eq!(snapshots[0].consumed, 0);
    assert!(
        h.periods(id).await.is_empty(),
        "the read wrote no period row"
    );

    held.release();
    debit.await.expect("join").expect("the debit succeeds");
    let rows = h.periods(id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].consumed, 5);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_debit_is_not_refused_while_a_read_is_in_flight() {
    let h = PgHarness::up().await;
    let first = h.create(consumption(tenant(), "u1", 100)).await;
    let second = h.create(consumption(tenant(), "u2", 100)).await;
    // A debit holds the second Quota's row, so a read that wrote period rows
    // would stop there after writing the first one's, still holding it.
    let (evaluate, mut held) = gated_evaluator();
    let blocker = tokio::spawn({
        let store = Arc::clone(&h.store);
        async move { debit_with(store, "u2", 5, "held", evaluate).await }
    });
    held.wait_until_reached().await;

    let read = tokio::spawn({
        let store = Arc::clone(&h.store);
        async move {
            store
                .bulk_read_quota_snapshot(
                    &ctx(),
                    &scope(),
                    &[applicable(tenant(), "u1"), applicable(tenant(), "u2")],
                    PageRequest::first(10),
                )
                .await
        }
    });
    // Until the read has finished or stopped on a lock.
    while !read.is_finished() && h.lock_waiters().await == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // At the 0 ms default a debit meeting a held row is refused at once.
    debit_with(Arc::clone(&h.store), "u1", 7, "first", allow_all())
        .await
        .expect("the debit is not refused");
    held.release();
    blocker
        .await
        .expect("join")
        .expect("the held debit succeeds");
    let page = read.await.expect("join").expect("the read succeeds");
    assert_eq!(of(&page, first).consumed, 0);
    assert_eq!(of(&page, second).consumed, 0);
    assert_eq!(h.periods(first).await[0].consumed, 7);
    assert_eq!(h.periods(second).await[0].consumed, 5);
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_issues_as_many_statements_for_twenty_quotas_as_for_two() {
    let h = PgHarness::up().await;
    let holders: Vec<String> = (0..10).map(|i| format!("u{i}")).collect();
    for holder in &holders {
        h.create(consumption(tenant(), holder, 100)).await;
        h.create(allocation(tenant(), holder, 100)).await;
    }
    let pairs: Vec<ApplicableQuotas> = holders
        .iter()
        .map(|holder| applicable(tenant(), holder))
        .collect();
    let (ctx, scope) = (ctx(), scope());
    let read = |pairs| {
        h.store
            .bulk_read_quota_snapshot(&ctx, &scope, pairs, PageRequest::first(100))
    };

    let (two, small) = Statements::during(read(&pairs[..1])).await;
    let (twenty, large) = Statements::during(read(&pairs)).await;
    assert_eq!(two.expect("read").items.len(), 2);
    assert_eq!(twenty.expect("read").items.len(), 20);
    assert!(small > 0, "the counter sees the read's statements");
    assert_eq!(
        large, small,
        "a page's statements do not grow with its Quotas"
    );
    h.down().await;
}
