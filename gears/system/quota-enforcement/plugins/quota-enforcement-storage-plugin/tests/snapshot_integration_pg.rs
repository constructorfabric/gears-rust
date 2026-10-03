#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed suite of the snapshot reads.
//!
//! What only a real concurrent backend can show:
//!
//! - a page is read from one snapshot: writers committing between its page
//!   query and its assembly change neither a counter nor a Quota row on it;
//! - a period row a writer commits after the read's snapshot is not
//!   duplicated: the read's own insert conflicts, the read runs again, and
//!   reports the committed row;
//! - under a tenant-subtree grant, compiled against `tenant_closure` by the
//!   secure layer, a visible stale Quota gets its row and an invisible one
//!   does not, through both reads;
//! - a read creates a row but never settles or emits: the debit that crosses
//!   the boundary emits the one `period-rollover`.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test snapshot_integration_pg`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, CapPatch, Decision, EvaluatedMutation, IdempotencyScope,
    IdempotencySubjectKey, IdempotencyWrite, MetricId, OperationType, PageRequest, PageResult,
    PayloadHash, PeriodType, PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft, QuotaId,
    QuotaPatch, QuotaSnapshot, QuotaType, SubjectRef, TenantId,
};
use sea_orm::EntityTrait as _;
use sea_orm_migration::prelude::*;
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

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(500).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Allow the requested amount against every applicable Quota.
fn allow_all() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        let plan = context
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
            .collect();
        Ok(EvaluationOutcome::validate(
            Decision::allowed_with_plan(plan),
            context,
        )?)
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
            _container: container,
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
        self.store
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
                    evaluate: allow_all(),
                },
                &[],
            )
            .await
            .expect("debit");
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
async fn a_period_row_committed_after_the_snapshot_is_reread_not_duplicated() {
    let h = PgHarness::up().await;
    let id = h.create(consumption(tenant(), "u1", 100)).await;
    let (reader, mut gate) = h.gated_reader();

    let read = tokio::spawn(async move {
        reader
            .read_quota_snapshot(&ctx(), &scope(), &applicable(tenant(), "u1"))
            .await
    });
    gate.wait_until_reached().await;
    // The reader saw no current row and will insert one; a debit creates it
    // first and commits.
    h.debit("u1", 7, "first").await;
    gate.release();
    let snapshots = read.await.expect("join").expect("the read succeeds");

    assert_eq!(h.periods(id).await.len(), 1, "one row for the window");
    assert_eq!(
        snapshots[0].consumed, 7,
        "the conflicting insert rolled the read back, and the retry saw the row"
    );
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn under_a_subtree_grant_both_reads_create_rows_only_for_what_they_see() {
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
    assert_eq!(h.periods(inside).await.len(), 1);
    assert!(h.periods(beyond).await.is_empty());

    h.set_now(DAY_TWO);
    let single = h
        .store
        .read_quota_snapshot(&ctx(), &subtree, &pairs[0])
        .await
        .expect("single read under the subtree grant");
    assert_eq!(single.len(), 1);
    assert_eq!(h.periods(inside).await.len(), 2);
    let none = h
        .store
        .read_quota_snapshot(&ctx(), &subtree, &pairs[1])
        .await
        .expect("single read of an invisible target");
    assert!(none.is_empty());
    assert!(h.periods(beyond).await.is_empty());
    h.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_opens_the_period_but_only_the_crossing_debit_settles_and_emits() {
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
    assert_eq!(read[0].consumed, 0);
    let rows = h.periods(id).await;
    assert_eq!(rows.len(), 2, "the read opened day two");
    assert!(
        rows.iter().all(|row| !row.is_settled),
        "and settled nothing"
    );
    assert_eq!(h.rollovers().await, before, "and emitted nothing");

    h.debit("u1", 1, "day-two").await;
    assert!(
        h.periods(id)
            .await
            .iter()
            .any(|row| row.is_settled && row.period_start < DAY_TWO),
        "the crossing debit settled day one"
    );
    assert_eq!(h.rollovers().await, before + 1, "exactly one rollover");
    h.down().await;
}
