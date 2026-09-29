#![allow(clippy::expect_used)]
//! The two snapshot reads on `SQLite`: the union each Quota once, keyset
//! pages, the scope (a tenant-subtree grant included), and the I3 exception —
//! the current window's row for a valid consumption Quota only, with nothing
//! settled or emitted.

use std::sync::{Arc, Mutex};

use gts::GtsTypeId;
use quota_enforcement_sdk::{
    ApplicableQuotas, PageRequest, PeriodType, QuotaDraft, QuotaId, QuotaType, StorageError,
    SubjectRef, TenantId, ValidityWindow,
};
use sea_orm::{ColumnTrait, EntityTrait as _};
use sea_orm_migration::prelude::*;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::Db;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::super::entity::quota_consumption_counter;
use crate::domain::ports::{ConsumptionStore, QuotaStore};
use crate::infra::storage::{SqlConsumptionStore, SqlQuotaStore};
use crate::test_support::{
    METRIC_TOKENS, actor, bound_outbox, draft, enqueued_messages, scope_for, tenant, test_db, user,
};

/// A Tuesday, inside an ordinary day period.
const DAY_ONE: OffsetDateTime = datetime!(2026-03-17 10:00:00 UTC);
/// The next day: the period of `DAY_ONE` has elapsed.
const DAY_TWO: OffsetDateTime = datetime!(2026-03-18 10:00:00 UTC);

const TENANT_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.tenant.v1~";

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("context")
}

fn tenant_subject(tenant: TenantId) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::try_new(TENANT_PROJECTION).expect("type id"),
        subject_id: tenant.as_uuid().to_string(),
    }
}

fn pair(tenant: TenantId, subjects: Vec<SubjectRef>) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant,
        subjects,
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

struct Harness {
    db: Db,
    store: SqlConsumptionStore,
    quotas: SqlQuotaStore,
    outbox: OutboxHandle,
    clock: Arc<Mutex<OffsetDateTime>>,
}

impl Harness {
    async fn up() -> Self {
        let db = test_db().await;
        let (outbox, enqueuer) = bound_outbox(&db).await;
        let clock = Arc::new(Mutex::new(DAY_ONE));
        let reader = Arc::clone(&clock);
        let store = SqlConsumptionStore::with_clock(
            db.clone(),
            Arc::clone(&enqueuer) as Arc<dyn crate::infra::outbox::NotificationEnqueuer>,
            Arc::new(move || *reader.lock().expect("clock")),
        );
        let quota_clock = Arc::clone(&clock);
        let quotas = SqlQuotaStore::with_clock(
            db.clone(),
            enqueuer,
            Arc::new(move || *quota_clock.lock().expect("clock")),
        );
        Self {
            db,
            store,
            quotas,
            outbox,
            clock,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }

    fn set_now(&self, now: OffsetDateTime) {
        *self.clock.lock().expect("clock") = now;
    }

    async fn create(&self, draft: QuotaDraft) -> QuotaId {
        let tenant = draft.tenant_id;
        self.quotas
            .create_quota(&actor(), &scope_for(tenant), draft, &[])
            .await
            .expect("create quota")
    }

    async fn allocation(&self, tenant: TenantId, subject: SubjectRef) -> QuotaId {
        let mut d = draft(tenant, "unused", Some(100));
        d.subject = subject;
        self.create(d).await
    }

    async fn consumption(&self, tenant: TenantId, subject: SubjectRef) -> QuotaId {
        self.create(consumption_draft(tenant, subject)).await
    }

    async fn periods(&self, id: QuotaId) -> Vec<quota_consumption_counter::Model> {
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

    async fn events(&self) -> usize {
        enqueued_messages(&self.db).await.len()
    }

    async fn bulk(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
    ) -> Result<quota_enforcement_sdk::PageResult<quota_enforcement_sdk::QuotaSnapshot>, StorageError>
    {
        self.store
            .bulk_read_quota_snapshot(&ctx(), scope, pairs, page)
            .await
    }
}

fn consumption_draft(tenant: TenantId, subject: SubjectRef) -> QuotaDraft {
    let mut d = draft(tenant, "unused", Some(100));
    d.subject = subject;
    d.quota_type = QuotaType::Consumption;
    d.period = Some(PeriodType::Day);
    d
}

fn ids(
    page: &quota_enforcement_sdk::PageResult<quota_enforcement_sdk::QuotaSnapshot>,
) -> Vec<QuotaId> {
    page.items
        .iter()
        .map(|snapshot| snapshot.quota_id)
        .collect()
}

// --- selection and pages -------------------------------------------------------

#[tokio::test]
async fn a_user_and_tenant_target_returns_both_tiers_once_and_a_tenant_target_only_its_own() {
    let h = Harness::up().await;
    let mine = h.allocation(tenant(), user("u1")).await;
    let theirs = h.allocation(tenant(), user("u2")).await;
    let shared = h.allocation(tenant(), tenant_subject(tenant())).await;

    // The user target and a repeated tenant target overlap on the tenant Quota.
    let page = h
        .bulk(
            &scope_for(tenant()),
            &[
                pair(tenant(), vec![tenant_subject(tenant()), user("u1")]),
                pair(tenant(), vec![tenant_subject(tenant())]),
            ],
            PageRequest::first(10),
        )
        .await
        .expect("bulk read");
    let mut expected = vec![mine, shared];
    expected.sort();
    assert_eq!(ids(&page), expected, "u1 and the tenant, each once");
    assert!(!ids(&page).contains(&theirs));

    let tenant_only = h
        .bulk(
            &scope_for(tenant()),
            &[pair(tenant(), vec![tenant_subject(tenant())])],
            PageRequest::first(10),
        )
        .await
        .expect("tenant-only read");
    assert_eq!(ids(&tenant_only), vec![shared]);
    h.down().await;
}

#[tokio::test]
async fn walking_the_cursor_yields_every_row_once_in_id_order() {
    let h = Harness::up().await;
    let mut all = Vec::new();
    for subject in ["u1", "u2", "u3", "u4", "u5"] {
        all.push(h.allocation(tenant(), user(subject)).await);
    }
    all.sort();
    let pairs = [pair(
        tenant(),
        ["u1", "u2", "u3", "u4", "u5"]
            .into_iter()
            .map(user)
            .collect(),
    )];

    let mut walked = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = h
            .bulk(
                &scope_for(tenant()),
                &pairs,
                PageRequest {
                    limit: 2,
                    cursor: cursor.clone(),
                },
            )
            .await
            .expect("page");
        pages += 1;
        walked.extend(ids(&page));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked, all);
    assert_eq!(pages, 3);

    let err = h
        .bulk(
            &scope_for(tenant()),
            &pairs,
            PageRequest {
                limit: 2,
                cursor: Some("not-a-cursor".to_owned()),
            },
        )
        .await
        .expect_err("a foreign cursor");
    assert_eq!(err, StorageError::InvalidCursor);
    h.down().await;
}

#[tokio::test]
async fn the_scope_narrows_the_page_and_deactivated_quotas_are_absent() {
    let h = Harness::up().await;
    let kept = h.allocation(tenant(), user("u1")).await;
    let gone = h.allocation(tenant(), user("u2")).await;
    h.quotas
        .deactivate_quota(&actor(), &scope_for(tenant()), gone, &[])
        .await
        .expect("deactivate");
    let pairs = [pair(tenant(), vec![user("u1"), user("u2")])];

    let page = h
        .bulk(&scope_for(tenant()), &pairs, PageRequest::first(10))
        .await
        .expect("read");
    assert_eq!(ids(&page), vec![kept]);
    let foreign = h
        .bulk(
            &AccessScope::for_tenant(Uuid::from_u128(0xbad)),
            &pairs,
            PageRequest::first(10),
        )
        .await
        .expect("read under another tenant's scope");
    assert!(
        foreign.items.is_empty(),
        "rows outside the scope are absent"
    );
    h.down().await;
}

#[tokio::test]
async fn allocation_and_consumption_quotas_report_their_own_shapes() {
    let h = Harness::up().await;
    let allocation = h.allocation(tenant(), user("u1")).await;
    let consumption = h.consumption(tenant(), user("u1")).await;

    let page = h
        .bulk(
            &scope_for(tenant()),
            &[pair(tenant(), vec![user("u1")])],
            PageRequest::first(10),
        )
        .await
        .expect("read");
    let of = |id: QuotaId| {
        page.items
            .iter()
            .find(|snapshot| snapshot.quota_id == id)
            .expect("snapshot")
    };
    assert_eq!(of(allocation).period, None, "no period for allocation");
    let period = of(consumption).period.expect("a consumption period");
    assert!(period.start <= DAY_ONE && DAY_ONE < period.end);
    assert_eq!(period.next_reset, period.end);
    assert_eq!(of(consumption).remaining, Some(100));
    h.down().await;
}

// --- the I3 exception --------------------------------------------------------------

#[tokio::test]
async fn a_read_creates_the_current_row_of_a_valid_quota_only_and_settles_nothing() {
    let h = Harness::up().await;
    let valid = h.consumption(tenant(), user("u1")).await;
    let lapsed = h
        .create(QuotaDraft {
            validity_window: Some(ValidityWindow {
                start: None,
                end: Some(datetime!(2026-03-18 00:00:00 UTC)),
            }),
            ..consumption_draft(tenant(), user("u2"))
        })
        .await;
    let pairs = [pair(tenant(), vec![user("u1"), user("u2")])];

    h.bulk(&scope_for(tenant()), &pairs, PageRequest::first(10))
        .await
        .expect("first read");
    assert_eq!(
        h.periods(valid).await.len(),
        1,
        "the missing row is created"
    );
    assert_eq!(
        h.periods(lapsed).await.len(),
        1,
        "on day one the second quota is still within its window"
    );
    let events = h.events().await;

    h.set_now(DAY_TWO);
    let page = h
        .bulk(&scope_for(tenant()), &pairs, PageRequest::first(10))
        .await
        .expect("read after the boundary");

    let rows = h.periods(valid).await;
    assert_eq!(rows.len(), 2, "the next window's row is created");
    assert!(rows.iter().all(|row| !row.is_settled), "nothing is settled");
    assert_eq!(h.events().await, events, "and nothing is emitted");
    assert_eq!(
        h.periods(lapsed).await.len(),
        1,
        "past its validity window it gets no new row"
    );
    let lapsed_snapshot = page
        .items
        .iter()
        .find(|snapshot| snapshot.quota_id == lapsed)
        .expect("an out-of-window quota is still returned");
    assert!(!lapsed_snapshot.currently_within_window);
    assert_eq!(lapsed_snapshot.consumed, 0);
    let window = lapsed_snapshot.period.expect("the computed current window");
    assert!(window.start <= DAY_TWO && DAY_TWO < window.end);

    // The single read behind preview follows the same rule.
    h.set_now(datetime!(2026-03-19 10:00:00 UTC));
    h.store
        .read_quota_snapshot(&ctx(), &scope_for(tenant()), &pairs[0])
        .await
        .expect("single read");
    assert_eq!(h.periods(valid).await.len(), 3);
    assert_eq!(h.periods(lapsed).await.len(), 1);
    h.down().await;
}

// --- a tenant-subtree grant ----------------------------------------------------------

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

fn subtree_of(root: TenantId) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_tenant_subtree(
        pep_properties::OWNER_TENANT_ID,
        root.as_uuid(),
        true,
        Vec::new(),
    )]))
}

#[tokio::test]
async fn under_a_subtree_grant_a_stale_valid_quota_gets_its_row_through_both_reads() {
    let h = Harness::up().await;
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
    let inside = h.consumption(child, user("u1")).await;
    let beyond = h.consumption(outside, user("u1")).await;
    let pairs = [
        pair(child, vec![user("u1")]),
        pair(outside, vec![user("u1")]),
    ];

    let page = h
        .bulk(&subtree_of(parent), &pairs, PageRequest::first(10))
        .await
        .expect("bulk read under the subtree grant");
    assert_eq!(
        ids(&page),
        vec![inside],
        "only the child's quota is visible"
    );
    assert_eq!(
        h.periods(inside).await.len(),
        1,
        "its current row was created"
    );
    assert!(
        h.periods(beyond).await.is_empty(),
        "the unseen quota got none"
    );

    h.set_now(DAY_TWO);
    let single = h
        .store
        .read_quota_snapshot(&ctx(), &subtree_of(parent), &pairs[0])
        .await
        .expect("single read under the subtree grant");
    assert_eq!(single.len(), 1);
    assert_eq!(
        h.periods(inside).await.len(),
        2,
        "the single read creates it too"
    );
    assert!(h.periods(beyond).await.is_empty());
    h.down().await;
}

// --- SQLite contention -------------------------------------------------------------

/// A file-backed WAL database with two connections, so a reader and a writer
/// really are concurrent; the in-memory test database has one connection.
async fn wal_db(path: &std::path::Path) -> Db {
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=WAL&busy_timeout=100",
        path.display()
    );
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(2),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("connect file sqlite");
    run_migrations_for_testing(&db, crate::infra::storage::Migrator::migrations())
        .await
        .expect("apply storage migrations");
    db
}

type Gated = (
    SqlConsumptionStore,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
);

/// A store over `db` whose first `times` clock readings — each taken after an
/// attempt's page query, so its snapshot is fixed — wait to be released.
fn gated_reader(db: &Db, enqueuer: &Arc<crate::QeOutbox>, times: u32) -> Gated {
    let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let remaining = Arc::new(Mutex::new(times));
    let go_rx = Arc::new(Mutex::new(go_rx));
    let reader = SqlConsumptionStore::with_clock(
        db.clone(),
        Arc::clone(enqueuer) as _,
        Arc::new(move || {
            let held = {
                let mut left = remaining.lock().expect("counter");
                let held = *left > 0;
                *left = left.saturating_sub(1);
                held
            };
            if held {
                reached_tx.send(()).expect("announce the gate");
                // Hand this worker's queued tasks to another thread while
                // waiting, or a task parked in its LIFO slot would starve.
                tokio::task::block_in_place(|| {
                    go_rx.lock().expect("gate").recv().expect("released");
                });
            }
            DAY_ONE
        }),
    );
    (reader, reached_rx, go_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_whose_snapshot_went_stale_before_its_insert_is_retried() {
    // Declared first so it is dropped last, after every handle on the file.
    let dir = tempfile::tempdir().expect("temp dir");
    let db = wal_db(&dir.path().join("snapshot.db")).await;
    let (outbox, enqueuer) = bound_outbox(&db).await;
    let quotas =
        SqlQuotaStore::with_clock(db.clone(), Arc::clone(&enqueuer) as _, Arc::new(|| DAY_ONE));
    let id = quotas
        .create_quota(
            &actor(),
            &scope_for(tenant()),
            consumption_draft(tenant(), user("u1")),
            &[],
        )
        .await
        .expect("create quota");
    let applicable = pair(tenant(), vec![user("u1")]);

    // Reader A stops at its clock, after its snapshot is fixed.
    let (reader, reached_rx, go_tx) = gated_reader(&db, &enqueuer, 1);
    let read_a = {
        let applicable = applicable.clone();
        tokio::spawn(async move {
            reader
                .read_quota_snapshot(&ctx(), &scope_for(tenant()), &applicable)
                .await
        })
    };
    tokio::task::spawn_blocking(move || reached_rx.recv())
        .await
        .expect("join")
        .expect("reader A reached its clock");

    // Reader B creates the row and commits while A holds its old snapshot.
    let writer = SqlConsumptionStore::with_clock(
        db.clone(),
        Arc::clone(&enqueuer) as _,
        Arc::new(|| DAY_ONE),
    );
    writer
        .read_quota_snapshot(&ctx(), &scope_for(tenant()), &applicable)
        .await
        .expect("reader B");
    go_tx.send(()).expect("release A");
    let snapshots = read_a
        .await
        .expect("join")
        .expect("reader A succeeds after a retry");

    assert_eq!(snapshots.len(), 1);
    let conn = db.conn().expect("connection");
    let rows = quota_consumption_counter::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(quota_consumption_counter::Column::QuotaId.eq(id.as_uuid())))
        .all(&conn)
        .await
        .expect("period rows");
    assert_eq!(rows.len(), 1, "one row for the window, not two");
    outbox.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_that_keeps_meeting_newer_commits_answers_unavailable() {
    // Declared first so it is dropped last, after every handle on the file.
    let dir = tempfile::tempdir().expect("temp dir");
    let db = wal_db(&dir.path().join("snapshot.db")).await;
    let (outbox, enqueuer) = bound_outbox(&db).await;
    let quotas =
        SqlQuotaStore::with_clock(db.clone(), Arc::clone(&enqueuer) as _, Arc::new(|| DAY_ONE));
    quotas
        .create_quota(
            &actor(),
            &scope_for(tenant()),
            consumption_draft(tenant(), user("u1")),
            &[],
        )
        .await
        .expect("create quota");
    let (reader, reached_rx, go_tx) = gated_reader(&db, &enqueuer, 3);
    let reached_rx = Arc::new(Mutex::new(reached_rx));
    let read = tokio::spawn(async move {
        reader
            .read_quota_snapshot(
                &ctx(),
                &scope_for(tenant()),
                &pair(tenant(), vec![user("u1")]),
            )
            .await
    });

    // Every attempt meets a commit made after its snapshot, so every insert
    // fails to upgrade.
    for attempt in 0..3 {
        let reached = Arc::clone(&reached_rx);
        tokio::task::spawn_blocking(move || reached.lock().expect("gate").recv())
            .await
            .expect("join")
            .expect("the attempt reached its clock");
        quotas
            .create_quota(
                &actor(),
                &scope_for(tenant()),
                draft(tenant(), &format!("other-{attempt}"), Some(1)),
                &[],
            )
            .await
            .expect("a concurrent commit");
        go_tx.send(()).expect("release the attempt");
    }

    let error = read.await.expect("join").expect_err("three conflicts");
    assert!(matches!(error, StorageError::Unavailable(_)), "{error:?}");
    outbox.stop().await;
}
