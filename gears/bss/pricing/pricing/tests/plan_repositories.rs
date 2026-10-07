//! Phase 3 repositories on `SQLite`: plans, revisions and items.
//!
//! Every conditional write is raced by two real writers on one database file, each on its own
//! connection, released together by a barrier: exactly one may win, and the loser must see the
//! write's own conflict code rather than a driver error or a second success.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::{
    domain::plan,
    infra::storage::{
        RepoError,
        entity::{plan as plan_e, plan_item, plan_revision, price_book, price_book_entry},
        repo::{
            approval_repo::PricingApprovalStore, book_repo, plan_item_repo, plan_repo,
            plan_revision_repo, price_book_entry_repo, price_repo,
        },
    },
};
use std::{future::Future, pin::Pin, sync::Arc};
use toolkit_db::secure::{AccessScope, TxConfig};
use toolkit_db::{ConnectOpts, DBProvider, Db, DbError, DbTx};
use uuid::Uuid;
mod storage_support;
use storage_support::{at, test_db};

fn book(tenant: Uuid) -> price_book::Model {
    price_book::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: format!("book-{}", Uuid::new_v4()),
        name: "Default EUR".into(),
        currency: "EUR".into(),
        valid_from: None,
        valid_until: None,
        description: None,
        version: 1,
        created_at: at(9),
        updated_at: at(9),
        archived_at: None,
        archived_by: None,
    }
}
fn entry(b: &price_book::Model, sku: Uuid) -> price_book_entry::Model {
    price_book_entry::Model {
        id: Uuid::new_v4(),
        tenant_id: b.tenant_id,
        book_id: b.id,
        sku_id: sku,
        charge_kind: "usage".into(),
        period: None,
        model: "per_unit".into(),
        usage_policy_id: None,
        usage_policy_version: None,
        usage_policy_digest: None,
        usage_sku_version: None,
        dimension_key: None,
        invoice_line_override: None,
        reservation_id: Uuid::new_v4(),
        reference_state: "confirmed".into(),
        version: 1,
        created_at: at(9),
        updated_at: at(9),
    }
}
fn plan(tenant: Uuid, code: &str) -> plan_e::Model {
    plan_e::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: code.into(),
        name: "Pro".into(),
        published_rev: None,
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
        work_revision_id: None,
        work_state: None,
        scheduled_revision_id: None,
        scheduled_from: None,
        published_revision_id: None,
        current_book_id: None,
        current_currency: None,
        last_activity_at: at(9),
    }
}
fn revision(p: &plan_e::Model, b: &price_book::Model, rev_no: i32) -> plan_revision::Model {
    plan_revision::Model {
        id: Uuid::new_v4(),
        tenant_id: p.tenant_id,
        plan_id: p.id,
        rev_no,
        book_id: b.id,
        state: "draft".into(),
        available_from: None,
        pending_unit_id: None,
        approved_by_unit_id: None,
        published_at: None,
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
    }
}
fn item(r: &plan_revision::Model, e: &price_book_entry::Model) -> plan_item::Model {
    plan_item::Model {
        id: Uuid::new_v4(),
        tenant_id: r.tenant_id,
        revision_id: r.id,
        sku_id: e.sku_id,
        price_book_entry_id: Some(e.id),
        treatment: "paid".into(),
        included_qty: None,
        qty_min: Some(1),
        reservation_id: None,
        reference_state: "unreserved".into(),
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
    }
}
fn date(s: &str) -> time::Date {
    time::Date::parse(s, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
/// A pending approval unit of `kind`, so a lock or a request can name it.
async fn unit(db: &DBProvider<DbError>, scope: &AccessScope, tenant: Uuid, kind: &str) -> Uuid {
    let id = Uuid::new_v4();
    let scope = scope.clone();
    let kind = kind.to_owned();
    price_repo::transaction(&db.db(), move |tx| {
        let scope = scope.clone();
        let kind = kind.clone();
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(
                tx,
                &Unit {
                    id,
                    tenant_id: tenant,
                    kind,
                    ref_type: "plan_revision".into(),
                    ref_id: Uuid::new_v4(),
                    state: UnitState::Pending,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: at(9),
                    submit_note: None,
                    decided_at: None,
                    decided_note: None,
                    snapshot: serde_json::json!({}),
                    snapshot_hash: "hash".into(),
                    version: 1,
                },
                &[],
            )
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
        })
    })
    .await
    .unwrap();
    id
}

// ---------------------------------------------------------------- the two-writer race

type Work = Arc<
    dyn for<'a> Fn(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<(), RepoError>> + Send + 'a>>
        + Send
        + Sync,
>;
fn db_error(e: &RepoError) -> Option<&sea_orm::DbErr> {
    match e {
        RepoError::Driver { source, .. } => Some(source),
        _ => None,
    }
}
async fn writer(db: Db, work: Work, barrier: Arc<tokio::sync::Barrier>) -> Result<(), RepoError> {
    let mut attempt = 0;
    db.transaction_with_retry(TxConfig::serializable(), db_error, move |tx| {
        attempt += 1;
        let first = attempt == 1;
        let barrier = Arc::clone(&barrier);
        let work = Arc::clone(&work);
        Box::pin(async move {
            if first {
                barrier.wait().await;
            }
            work(tx).await
        })
    })
    .await
}
/// Run `work` on two connections to one file at once; exactly one wins and the other sees `code`.
async fn exactly_one_wins(db: &DBProvider<DbError>, dsn: &str, code: &str, work: Work) {
    let other = toolkit_db::connect_db(
        dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (a, b) = tokio::join!(
        writer(db.db(), Arc::clone(&work), Arc::clone(&barrier)),
        writer(other, work, barrier)
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "{code}: {a:?} / {b:?}"
    );
    let loser = if let Err(e) = a { e } else { b.unwrap_err() };
    assert!(
        matches!(&loser, RepoError::Conflict { code: c } if *c == code),
        "{code}: the loser saw {loser:?}"
    );
}
fn lost(won: bool) -> Result<(), RepoError> {
    if won {
        Ok(())
    } else {
        Err(RepoError::Conflict { code: "LOCK_LOST" })
    }
}
fn conflict<T: std::fmt::Debug>(result: Result<T, RepoError>, code: &str) {
    match result {
        Err(RepoError::Conflict { code: c }) if c == code => {}
        other => panic!("expected {code}, got {other:?}"),
    }
}

/// A tenant with a book, an entry, a plan and a draft rev 1, all on one file.
struct World {
    db: DBProvider<DbError>,
    scope: AccessScope,
    tenant: Uuid,
    /// Held for the test's life: its temporary directory holds the database.
    dsn: storage_support::TestDsn,
    book: price_book::Model,
    entry: price_book_entry::Model,
    plan: plan_e::Model,
    revision: plan_revision::Model,
}
async fn world() -> World {
    let (db, scope, tenant, dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let b = book_repo::insert(&conn, &scope, book(tenant))
        .await
        .unwrap();
    let e = price_book_entry_repo::insert(&conn, &scope, entry(&b, Uuid::new_v4()))
        .await
        .unwrap();
    let p = plan_repo::insert(&conn, &scope, plan(tenant, "pro"))
        .await
        .unwrap();
    let r = plan_revision_repo::insert(&conn, &scope, revision(&p, &b, 1))
        .await
        .unwrap();
    // The revision insert refreshes the plan summary (D-484).
    let p = plan_repo::find(&conn, &scope, tenant, p.id)
        .await
        .unwrap()
        .unwrap();
    World {
        db,
        scope,
        tenant,
        dsn,
        book: b,
        entry: e,
        plan: p,
        revision: r,
    }
}

// ---------------------------------------------------------------- plans

#[tokio::test]
async fn plan_round_trips_and_its_code_is_unique_per_tenant() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    assert_eq!(
        plan_repo::find(&conn, &w.scope, w.tenant, w.plan.id)
            .await
            .unwrap(),
        Some(w.plan.clone())
    );
    conflict(
        plan_repo::insert(&conn, &w.scope, plan(w.tenant, "pro")).await,
        "PLAN_CODE_TAKEN",
    );
    let other = plan_repo::insert(&conn, &w.scope, plan(w.tenant, "basic"))
        .await
        .unwrap();
    let listed: Vec<_> = plan_repo::list(&conn, &w.scope, w.tenant)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.code)
        .collect();
    assert_eq!(listed, vec!["basic".to_owned(), "pro".to_owned()]);
    // Another tenant may reuse the code and cannot read this tenant's plan.
    let foreign = Uuid::new_v4();
    let foreign_scope = AccessScope::for_tenant(foreign);
    plan_repo::insert(&conn, &foreign_scope, plan(foreign, "pro"))
        .await
        .unwrap();
    assert!(
        plan_repo::find(&conn, &foreign_scope, w.tenant, other.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn plan_rename_is_a_conditional_write() {
    let w = world().await;
    let (tenant, id, scope) = (w.tenant, w.plan.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                plan_repo::rename(tx, &scope, tenant, id, 1, "Pro 2".into(), at(10)).await
            })
        }),
    )
    .await;
    let got = plan_repo::find(&w.db.conn().unwrap(), &w.scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((got.name.as_str(), got.version), ("Pro 2", 2));
}

#[tokio::test]
async fn plan_publish_projection_is_a_conditional_write() {
    let w = world().await;
    let (tenant, id, scope) = (w.tenant, w.plan.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(
                async move { plan_repo::set_published(tx, &scope, tenant, id, 1, 3, at(10)).await },
            )
        }),
    )
    .await;
    assert_eq!(
        plan_repo::find(&w.db.conn().unwrap(), &w.scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .published_rev,
        Some(3)
    );
}

// ---------------------------------------------------------------- revisions

#[tokio::test]
async fn revision_parents_are_tenant_scoped() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let foreign = Uuid::new_v4();
    let foreign_scope = AccessScope::for_tenant(foreign);
    let mut r = revision(&w.plan, &w.book, 2);
    r.tenant_id = foreign;
    conflict(
        plan_revision_repo::insert(&conn, &foreign_scope, r).await,
        "PLAN_NOT_FOUND",
    );
    let theirs = plan_repo::insert(&conn, &foreign_scope, plan(foreign, "pro"))
        .await
        .unwrap();
    // Their plan, our book.
    conflict(
        plan_revision_repo::insert(&conn, &foreign_scope, revision(&theirs, &w.book, 1)).await,
        "BOOK_NOT_FOUND",
    );
    assert!(
        plan_revision_repo::find(&conn, &foreign_scope, w.tenant, w.revision.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn one_open_and_one_published_revision_per_plan_and_rev_numbers_are_unique() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    // A superseded rev 1 joins no partial index, so only the rev_no key can refuse it.
    let mut same_no = revision(&w.plan, &w.book, 1);
    same_no.state = "superseded".into();
    conflict(
        plan_revision_repo::insert(&conn, &w.scope, same_no).await,
        "REVISION_NO_TAKEN",
    );
    // rev 1 is the open draft: neither a second draft nor a pending revision may join it.
    conflict(
        plan_revision_repo::insert(&conn, &w.scope, revision(&w.plan, &w.book, 2)).await,
        "REVISION_DRAFT_EXISTS",
    );
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    let mut pending = revision(&w.plan, &w.book, 2);
    pending.state = "pending".into();
    pending.pending_unit_id = Some(u);
    conflict(
        plan_revision_repo::insert(&conn, &w.scope, pending).await,
        "REVISION_DRAFT_EXISTS",
    );
    // Published and superseded revisions sit beside the open one; only one is published.
    let mut published = revision(&w.plan, &w.book, 2);
    published.state = "published".into();
    plan_revision_repo::insert(&conn, &w.scope, published)
        .await
        .unwrap();
    let mut second = revision(&w.plan, &w.book, 3);
    second.state = "published".into();
    conflict(
        plan_revision_repo::insert(&conn, &w.scope, second).await,
        "REVISION_PUBLISHED_EXISTS",
    );
    for rev_no in [4, 5] {
        let mut old = revision(&w.plan, &w.book, rev_no);
        old.state = "superseded".into();
        plan_revision_repo::insert(&conn, &w.scope, old)
            .await
            .unwrap();
    }
    let numbers: Vec<_> = plan_revision_repo::for_plan(&conn, &w.scope, w.tenant, w.plan.id)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.rev_no)
        .collect();
    assert_eq!(numbers, vec![1, 2, 4, 5]);
}

#[tokio::test]
async fn revision_state_is_the_enums_vocabulary_and_round_trips() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let other = plan_repo::insert(&conn, &w.scope, plan(w.tenant, "other"))
        .await
        .unwrap();
    let mut odd = revision(&other, &w.book, 1);
    odd.state = "retired".into();
    assert!(
        plan_revision_repo::insert(&conn, &w.scope, odd)
            .await
            .is_err()
    );
    let mut dated = revision(&other, &w.book, 1);
    dated.available_from = Some(date("2026-10-01"));
    assert_eq!(
        plan_revision_repo::insert(&conn, &w.scope, dated.clone())
            .await
            .unwrap(),
        dated
    );
}

#[tokio::test]
async fn revision_draft_edit_is_a_conditional_write_on_an_unlocked_draft() {
    let w = world().await;
    let mut edited = w.revision.clone();
    edited.available_from = Some(date("2026-10-01"));
    let scope = w.scope.clone();
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            let edited = edited.clone();
            Box::pin(async move { plan_revision_repo::update_draft(tx, &scope, edited).await })
        }),
    )
    .await;
    let conn = w.db.conn().unwrap();
    let got = plan_revision_repo::find(&conn, &w.scope, w.tenant, w.revision.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (got.available_from, got.version),
        (Some(date("2026-10-01")), 2)
    );
    // A book of another tenant is refused before the write.
    let foreign = Uuid::new_v4();
    let theirs = book_repo::insert(&conn, &AccessScope::for_tenant(foreign), book(foreign))
        .await
        .unwrap();
    let mut moved = got.clone();
    moved.book_id = theirs.id;
    conflict(
        plan_revision_repo::update_draft(&conn, &w.scope, moved).await,
        "BOOK_NOT_FOUND",
    );
    // A locked revision is no longer editable.
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 2)
            .await
            .unwrap()
    );
    let mut late = got;
    late.version = 3;
    conflict(
        plan_revision_repo::update_draft(&conn, &w.scope, late).await,
        "STALE_REVISION",
    );
}

#[tokio::test]
async fn revision_lock_is_conditional_and_names_an_existing_unit() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    conflict(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, Uuid::new_v4(), 1)
            .await,
        "UNIT_NOT_FOUND",
    );
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    let (tenant, id, scope) = (w.tenant, w.revision.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "LOCK_LOST",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                lost(plan_revision_repo::try_lock(tx, &scope, tenant, id, u, 1).await?)
            })
        }),
    )
    .await;
    let got = plan_revision_repo::find(&conn, &w.scope, w.tenant, w.revision.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (got.state.as_str(), got.pending_unit_id, got.version),
        ("pending", Some(u), 2)
    );
}

#[tokio::test]
async fn revision_unlock_returns_the_owning_units_revision_to_draft() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 1)
            .await
            .unwrap()
    );
    conflict(
        plan_revision_repo::unlock(&conn, &w.scope, w.tenant, w.revision.id, Uuid::new_v4()).await,
        "STALE_REVISION",
    );
    let (tenant, id, scope) = (w.tenant, w.revision.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move { plan_revision_repo::unlock(tx, &scope, tenant, id, u).await })
        }),
    )
    .await;
    let got = plan_revision_repo::find(&conn, &w.scope, w.tenant, w.revision.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (got.state.as_str(), got.pending_unit_id, got.version),
        ("draft", None, 3)
    );
}

#[tokio::test]
async fn revision_publish_is_conditional_on_the_owning_unit_and_one_published_per_plan() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 1)
            .await
            .unwrap()
    );
    conflict(
        plan_revision_repo::publish(
            &conn,
            &w.scope,
            w.tenant,
            w.revision.id,
            Uuid::new_v4(),
            at(10),
        )
        .await,
        "REVISION_NOT_PENDING",
    );
    let (tenant, id, scope) = (w.tenant, w.revision.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "REVISION_NOT_PENDING",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(
                async move { plan_revision_repo::publish(tx, &scope, tenant, id, u, at(10)).await },
            )
        }),
    )
    .await;
    let got = plan_revision_repo::find(&conn, &w.scope, w.tenant, w.revision.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            got.state.as_str(),
            got.pending_unit_id,
            got.approved_by_unit_id,
            got.published_at
        ),
        ("published", None, Some(u), Some(at(10)))
    );
    // A second revision cannot publish beside it until it is superseded.
    let next = plan_revision_repo::insert(&conn, &w.scope, revision(&w.plan, &w.book, 2))
        .await
        .unwrap();
    let u2 = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, next.id, u2, 1)
            .await
            .unwrap()
    );
    conflict(
        plan_revision_repo::publish(&conn, &w.scope, w.tenant, next.id, u2, at(11)).await,
        "REVISION_PUBLISHED_EXISTS",
    );
    plan_revision_repo::supersede(
        &conn,
        &w.scope,
        w.tenant,
        w.revision.id,
        got.version,
        at(11),
    )
    .await
    .unwrap();
    plan_revision_repo::publish(&conn, &w.scope, w.tenant, next.id, u2, at(11))
        .await
        .unwrap();
}

#[tokio::test]
async fn revision_supersede_is_a_conditional_write_on_a_published_revision() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    // A draft is not superseded.
    conflict(
        plan_revision_repo::supersede(&conn, &w.scope, w.tenant, w.revision.id, 1, at(10)).await,
        "STALE_REVISION",
    );
    let mut published = revision(&w.plan, &w.book, 2);
    published.state = "published".into();
    let published = plan_revision_repo::insert(&conn, &w.scope, published)
        .await
        .unwrap();
    let (tenant, id, scope) = (w.tenant, published.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                plan_revision_repo::supersede(tx, &scope, tenant, id, 1, at(10)).await
            })
        }),
    )
    .await;
    assert_eq!(
        plan_revision_repo::find(&conn, &w.scope, w.tenant, published.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "superseded"
    );
}

#[tokio::test]
async fn revision_delete_is_a_conditional_write_on_an_empty_unlocked_draft() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let i = plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry))
        .await
        .unwrap();
    // Items go first: the revision refuses to leave them behind.
    conflict(
        plan_revision_repo::delete_draft(&conn, &w.scope, w.tenant, w.revision.id, 1).await,
        "STALE_REVISION",
    );
    plan_item_repo::delete_draft(&conn, &w.scope, w.tenant, i.id, 1)
        .await
        .unwrap();
    let (tenant, id, scope) = (w.tenant, w.revision.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(
                async move { plan_revision_repo::delete_draft(tx, &scope, tenant, id, 1).await },
            )
        }),
    )
    .await;
    assert!(
        plan_revision_repo::find(&conn, &w.scope, w.tenant, w.revision.id)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------- scheduled revisions (D-448)

/// `hour` o'clock UTC on `day`.
fn instant(day: &str, hour: u8) -> time::OffsetDateTime {
    date(day).with_hms(hour, 0, 0).unwrap().assume_utc()
}
async fn find_revision(w: &World, id: Uuid) -> plan_revision::Model {
    plan_revision_repo::find(&w.db.conn().unwrap(), &w.scope, w.tenant, id)
        .await
        .unwrap()
        .unwrap()
}
async fn find_plan(w: &World, id: Uuid) -> plan_e::Model {
    plan_repo::find(&w.db.conn().unwrap(), &w.scope, w.tenant, id)
        .await
        .unwrap()
        .unwrap()
}
/// A new unit of the tenant locks the draft at `version`.
async fn locked(w: &World, id: Uuid, version: i64) -> Uuid {
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&w.db.conn().unwrap(), &w.scope, w.tenant, id, u, version)
            .await
            .unwrap()
    );
    u
}
/// The world's rev 1 published through its unit at 09:00 (the plan's `published_rev` 1, its
/// version 2), and rev 2 the next change: a draft available from `from`.
async fn changing(w: &World, from: &str) -> plan_revision::Model {
    let conn = w.db.conn().unwrap();
    let u = locked(w, w.revision.id, 1).await;
    plan_revision_repo::publish(&conn, &w.scope, w.tenant, w.revision.id, u, at(9))
        .await
        .unwrap();
    plan_repo::set_published(&conn, &w.scope, w.tenant, w.plan.id, 1, 1, at(9))
        .await
        .unwrap();
    let mut next = revision(&w.plan, &w.book, 2);
    next.available_from = Some(date(from));
    plan_revision_repo::insert(&conn, &w.scope, next)
        .await
        .unwrap()
}
/// A switch that finds nothing due reads as the loser of a race.
fn switched(s: Option<plan_revision_repo::Switched>) -> Result<(), RepoError> {
    if s.is_some() {
        Ok(())
    } else {
        Err(RepoError::Conflict {
            code: "NOTHING_DUE",
        })
    }
}

/// `schedule` moves only a pending revision its own unit holds; the lock turns into
/// `approved_by_unit_id`, `published_at` stays null, and the published revision and the plan's
/// projection do not move. A plan holds one scheduled revision.
#[tokio::test]
async fn revision_schedule_is_conditional_on_the_owning_unit_and_one_scheduled_per_plan() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let next = changing(&w, "2026-10-01").await;
    let early = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    conflict(
        plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, early, at(10)).await,
        "REVISION_NOT_PENDING",
    );
    let u = locked(&w, next.id, 1).await;
    conflict(
        plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, Uuid::new_v4(), at(10))
            .await,
        "REVISION_NOT_PENDING",
    );
    let (tenant, id, scope) = (w.tenant, next.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "REVISION_NOT_PENDING",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                plan_revision_repo::schedule(tx, &scope, tenant, id, u, at(10)).await
            })
        }),
    )
    .await;
    let got = find_revision(&w, next.id).await;
    assert_eq!(
        (
            got.state.as_str(),
            got.pending_unit_id,
            got.approved_by_unit_id,
            got.published_at,
            got.version,
            got.updated_at
        ),
        ("scheduled", None, Some(u), None, 3, at(10))
    );
    let published = find_revision(&w, w.revision.id).await;
    assert_eq!(
        (published.state.as_str(), published.version),
        ("published", 3)
    );
    let p = find_plan(&w, w.plan.id).await;
    assert_eq!((p.published_rev, p.version), (Some(1), 2));
    // A published or a scheduled revision is not scheduled again.
    conflict(
        plan_revision_repo::schedule(&conn, &w.scope, w.tenant, w.revision.id, u, at(11)).await,
        "REVISION_NOT_PENDING",
    );
    conflict(
        plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, u, at(11)).await,
        "REVISION_NOT_PENDING",
    );
    // One scheduled revision per plan: a third one, locked, meets the index on SQLite's
    // column-only message, told apart by the state the write sets.
    let mut third = revision(&w.plan, &w.book, 3);
    third.available_from = Some(date("2026-11-01"));
    let third = plan_revision_repo::insert(&conn, &w.scope, third)
        .await
        .unwrap();
    let u3 = locked(&w, third.id, 1).await;
    conflict(
        plan_revision_repo::schedule(&conn, &w.scope, w.tenant, third.id, u3, at(11)).await,
        "REVISION_SCHEDULED_EXISTS",
    );
    let mut direct = revision(&w.plan, &w.book, 4);
    direct.state = "scheduled".into();
    direct.approved_by_unit_id = Some(u3);
    direct.available_from = Some(date("2026-12-01"));
    conflict(
        plan_revision_repo::insert(&conn, &w.scope, direct).await,
        "REVISION_SCHEDULED_EXISTS",
    );
}

/// `switch_due` persists the plan's due switch in one transaction: the published revision is
/// superseded, the scheduled one published from 00:00 UTC of its date, and `published_rev`
/// advanced WITHOUT the plan's version or `updated_at` moving; the revision rows bump as usual. A
/// second call switches nothing and writes nothing.
#[tokio::test]
async fn revision_switch_due_publishes_the_due_revision_once_and_leaves_the_plan_version() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    // A plan with nothing scheduled has nothing to switch.
    assert_eq!(
        plan_revision_repo::switch_due(&conn, &w.scope, w.tenant, w.plan.id, at(10))
            .await
            .unwrap(),
        None
    );
    let next = changing(&w, "2026-10-01").await;
    let u = locked(&w, next.id, 1).await;
    plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, u, at(10))
        .await
        .unwrap();
    // The day before its date, to the last second, it is not due.
    let eve = date("2026-09-30")
        .with_hms(23, 59, 59)
        .unwrap()
        .assume_utc();
    assert_eq!(
        plan_revision_repo::switch_due(&conn, &w.scope, w.tenant, w.plan.id, eve)
            .await
            .unwrap(),
        None
    );
    assert_eq!(find_revision(&w, next.id).await.state, "scheduled");
    let now = instant("2026-10-01", 8);
    let got = plan_revision_repo::switch_due(&conn, &w.scope, w.tenant, w.plan.id, now)
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(plan_revision_repo::Switched {
            superseded_revision_id: Some(w.revision.id),
            revision_id: next.id,
            unit_id: u,
            rev_no: 2,
            book_id: w.book.id,
        })
    );
    let old = find_revision(&w, w.revision.id).await;
    assert_eq!(
        (
            old.state.as_str(),
            old.published_at,
            old.version,
            old.updated_at
        ),
        ("superseded", Some(at(9)), 4, now),
        "the predecessor keeps the instant it was published at"
    );
    let new = find_revision(&w, next.id).await;
    assert_eq!(
        (
            new.state.as_str(),
            new.published_at,
            new.pending_unit_id,
            new.approved_by_unit_id,
            new.version,
            new.updated_at
        ),
        (
            "published",
            Some(instant("2026-10-01", 0)),
            None,
            Some(u),
            4,
            now
        )
    );
    let p = find_plan(&w, w.plan.id).await;
    assert_eq!(
        (p.published_rev, p.version, p.updated_at),
        (Some(2), 2, at(9)),
        "published_rev is a projection: the plan's version and updated_at stay"
    );
    // Idempotent: nothing is due any more, and nothing is written.
    assert_eq!(
        plan_revision_repo::switch_due(
            &conn,
            &w.scope,
            w.tenant,
            w.plan.id,
            instant("2026-10-02", 8)
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(find_revision(&w, next.id).await.version, 4);
    assert_eq!(find_revision(&w, w.revision.id).await.version, 4);
    assert_eq!(find_plan(&w, w.plan.id).await.version, 2);
    // The plan's rename If-Match read before the switch is still good.
    plan_repo::rename(&conn, &w.scope, w.tenant, w.plan.id, 2, "Pro 2".into(), now)
        .await
        .unwrap();
}

/// A plan's first revision scheduled: the switch publishes it with nothing to supersede.
#[tokio::test]
async fn revision_switch_due_of_a_first_revision_supersedes_nothing() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let mut dated = w.revision.clone();
    dated.available_from = Some(date("2026-10-01"));
    plan_revision_repo::update_draft(&conn, &w.scope, dated)
        .await
        .unwrap();
    let u = locked(&w, w.revision.id, 2).await;
    plan_revision_repo::schedule(&conn, &w.scope, w.tenant, w.revision.id, u, at(10))
        .await
        .unwrap();
    assert_eq!(find_plan(&w, w.plan.id).await.published_rev, None);
    let got = plan_revision_repo::switch_due(
        &conn,
        &w.scope,
        w.tenant,
        w.plan.id,
        instant("2026-10-03", 1),
    )
    .await
    .unwrap();
    assert_eq!(
        got,
        Some(plan_revision_repo::Switched {
            superseded_revision_id: None,
            revision_id: w.revision.id,
            unit_id: u,
            rev_no: 1,
            book_id: w.book.id,
        })
    );
    let p = find_plan(&w, w.plan.id).await;
    assert_eq!((p.published_rev, p.version), (Some(1), 1));
    assert_eq!(
        find_revision(&w, w.revision.id).await.published_at,
        Some(instant("2026-10-01", 0))
    );
}

/// A scheduled row that names no approving unit has nothing an event could name: the switch
/// refuses it as a corrupt row and writes nothing. No write of the gear makes one; it is inserted
/// here to isolate the refusal.
#[tokio::test]
async fn revision_switch_due_refuses_a_scheduled_revision_without_its_unit() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let other = plan_repo::insert(&conn, &w.scope, plan(w.tenant, "orphan"))
        .await
        .unwrap();
    let mut orphan = revision(&other, &w.book, 1);
    orphan.state = "scheduled".into();
    orphan.available_from = Some(date("2026-10-01"));
    let orphan = plan_revision_repo::insert(&conn, &w.scope, orphan)
        .await
        .unwrap();
    let refused = plan_revision_repo::switch_due(
        &conn,
        &w.scope,
        w.tenant,
        other.id,
        instant("2026-10-02", 0),
    )
    .await;
    assert!(
        matches!(&refused, Err(RepoError::CorruptRow(m)) if m.contains(&orphan.id.to_string())),
        "{refused:?}"
    );
    let got = find_revision(&w, orphan.id).await;
    assert_eq!((got.state.as_str(), got.version), ("scheduled", 1));
    assert_eq!(find_plan(&w, other.id).await.published_rev, None);
}

/// Two writers switch one plan at once: exactly one switches, the other finds nothing due and
/// writes nothing; the plan ends with one published revision.
#[tokio::test]
async fn revision_switch_due_racing_itself_switches_once() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let next = changing(&w, "2026-10-01").await;
    let u = locked(&w, next.id, 1).await;
    plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, u, at(10))
        .await
        .unwrap();
    let (tenant, plan_id, scope) = (w.tenant, w.plan.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "NOTHING_DUE",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                switched(
                    plan_revision_repo::switch_due(
                        tx,
                        &scope,
                        tenant,
                        plan_id,
                        instant("2026-10-01", 9),
                    )
                    .await?,
                )
            })
        }),
    )
    .await;
    let states: Vec<(i32, String, i64)> =
        plan_revision_repo::for_plan(&conn, &w.scope, w.tenant, w.plan.id)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.rev_no, r.state, r.version))
            .collect();
    assert_eq!(
        states,
        [
            (1, "superseded".to_owned(), 4),
            (2, "published".to_owned(), 4)
        ]
    );
    let p = find_plan(&w, w.plan.id).await;
    assert_eq!((p.published_rev, p.version), (Some(2), 2));
}

/// `unschedule` returns a scheduled revision to an unlocked draft only BEFORE its date, with
/// `approved_by_unit_id` cleared, conditional on its state (no If-Match, D-448). A draft beside
/// it would break the one-open-revision index, which refuses it.
#[tokio::test]
async fn revision_unschedule_returns_a_scheduled_revision_before_its_date_to_an_unlocked_draft() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let next = changing(&w, "2026-10-01").await;
    conflict(
        plan_revision_repo::unschedule(&conn, &w.scope, w.tenant, next.id, at(10)).await,
        "REVISION_NOT_SCHEDULED",
    );
    let u = locked(&w, next.id, 1).await;
    conflict(
        plan_revision_repo::unschedule(&conn, &w.scope, w.tenant, next.id, at(10)).await,
        "REVISION_NOT_SCHEDULED",
    );
    conflict(
        plan_revision_repo::unschedule(&conn, &w.scope, w.tenant, w.revision.id, at(10)).await,
        "REVISION_NOT_SCHEDULED",
    );
    plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, u, at(10))
        .await
        .unwrap();
    // From 00:00 UTC of its date it is due, and no longer withdrawn.
    conflict(
        plan_revision_repo::unschedule(
            &conn,
            &w.scope,
            w.tenant,
            next.id,
            instant("2026-10-01", 0),
        )
        .await,
        "REVISION_NOT_SCHEDULED",
    );
    let (tenant, id, scope) = (w.tenant, next.id, w.scope.clone());
    let eve = instant("2026-09-30", 23);
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "REVISION_NOT_SCHEDULED",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(
                async move { plan_revision_repo::unschedule(tx, &scope, tenant, id, eve).await },
            )
        }),
    )
    .await;
    let got = find_revision(&w, next.id).await;
    assert_eq!(
        (
            got.state.as_str(),
            got.pending_unit_id,
            got.approved_by_unit_id,
            got.published_at,
            got.version,
            got.updated_at
        ),
        ("draft", None, None, None, 4, eve)
    );
    assert_eq!(find_revision(&w, w.revision.id).await.state, "published");
    // A draft again: it is edited, locked and scheduled again as any draft is.
    let mut edited = got.clone();
    edited.available_from = Some(date("2026-10-15"));
    plan_revision_repo::update_draft(&conn, &w.scope, edited)
        .await
        .unwrap();
    let u2 = locked(&w, next.id, 5).await;
    plan_revision_repo::schedule(&conn, &w.scope, w.tenant, next.id, u2, at(11))
        .await
        .unwrap();
    // A draft beside the scheduled revision (M1's invariant, held at the storage too).
    plan_revision_repo::insert(&conn, &w.scope, revision(&w.plan, &w.book, 3))
        .await
        .unwrap();
    conflict(
        plan_revision_repo::unschedule(&conn, &w.scope, w.tenant, next.id, at(12)).await,
        "REVISION_DRAFT_EXISTS",
    );
    assert_eq!(find_revision(&w, next.id).await.state, "scheduled");
}

/// The job's scan: every tenant's due scheduled revisions, by date then id, bounded; a revision
/// not yet due and a revision in another state are not in it.
#[tokio::test]
async fn due_scheduled_scans_every_tenant_by_date_then_id() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let other = Uuid::new_v4();
    let other_scope = AccessScope::for_tenant(other);
    let their_book = book_repo::insert(&conn, &other_scope, book(other))
        .await
        .unwrap();
    let mut due = Vec::new();
    // Fixed ids, the later date carrying the smallest: an order by id alone is not this one.
    for (n, tenant, scope, b, code, state, from) in [
        (
            1,
            w.tenant,
            &w.scope,
            &w.book,
            "a",
            "scheduled",
            "2026-10-02",
        ),
        (
            3,
            w.tenant,
            &w.scope,
            &w.book,
            "b",
            "scheduled",
            "2026-10-01",
        ),
        (
            4,
            w.tenant,
            &w.scope,
            &w.book,
            "c",
            "scheduled",
            "2026-10-05",
        ),
        (
            5,
            w.tenant,
            &w.scope,
            &w.book,
            "d",
            "published",
            "2026-09-01",
        ),
        (
            2,
            other,
            &other_scope,
            &their_book,
            "e",
            "scheduled",
            "2026-10-01",
        ),
    ] {
        let p = plan_repo::insert(&conn, scope, plan(tenant, code))
            .await
            .unwrap();
        let u = unit(&w.db, scope, tenant, "plan_revision").await;
        let mut r = revision(&p, b, 1);
        r.id = Uuid::from_u128(0x0017_0000 + n);
        r.state = state.into();
        r.approved_by_unit_id = Some(u);
        r.available_from = Some(date(from));
        let r = plan_revision_repo::insert(&conn, scope, r).await.unwrap();
        if state == "scheduled" && from <= "2026-10-03" {
            due.push((from, r.id, tenant));
        }
    }
    due.sort();
    let scan = |today: &str, limit: u64| {
        let conn = w.db.conn().unwrap();
        let today = date(today);
        async move {
            plan_revision_repo::due_scheduled(&conn, today, limit)
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.id, r.tenant_id))
                .collect::<Vec<_>>()
        }
    };
    let all: Vec<(Uuid, Uuid)> = due.iter().map(|(_, id, t)| (*id, *t)).collect();
    assert_eq!(
        all.iter()
            .map(|(id, _)| id.as_u128() - 0x0017_0000)
            .collect::<Vec<_>>(),
        [2, 3, 1],
        "by date, then id"
    );
    assert_eq!(scan("2026-10-03", 10).await, all);
    assert_eq!(scan("2026-10-03", 2).await, all[..2]);
    assert!(scan("2026-09-30", 10).await.is_empty());
}

// ---------------------------------------------------------------- items

#[tokio::test]
async fn item_parents_are_tenant_scoped_and_the_revision_must_be_an_unlocked_draft() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let foreign = Uuid::new_v4();
    let foreign_scope = AccessScope::for_tenant(foreign);
    let mut theirs = item(&w.revision, &w.entry);
    theirs.tenant_id = foreign;
    conflict(
        plan_item_repo::insert_as_given(&conn, &foreign_scope, theirs).await,
        "REVISION_NOT_FOUND",
    );
    let mut ghost = item(&w.revision, &w.entry);
    ghost.price_book_entry_id = Some(Uuid::new_v4());
    conflict(
        plan_item_repo::insert_as_given(&conn, &w.scope, ghost).await,
        "ENTRY_NOT_FOUND",
    );
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 1)
            .await
            .unwrap()
    );
    conflict(
        plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry)).await,
        "REVISION_NOT_DRAFT",
    );
}

/// An item's entry must be an entry of its revision's book for the item's own SKU (D-407): the
/// insert re-reads both in the caller's transaction, so every writer is held to it.
#[tokio::test]
async fn item_entry_must_be_of_the_revisions_book_and_the_items_sku() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let other = book_repo::insert(&conn, &w.scope, book(w.tenant))
        .await
        .unwrap();
    let foreign = price_book_entry_repo::insert(&conn, &w.scope, entry(&other, w.entry.sku_id))
        .await
        .unwrap();
    conflict(
        plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &foreign)).await,
        "ITEM_BOOK_FOREIGN",
    );
    let mut mismatched = item(&w.revision, &w.entry);
    mismatched.sku_id = Uuid::new_v4();
    conflict(
        plan_item_repo::insert_as_given(&conn, &w.scope, mismatched).await,
        "ITEM_ENTRY_SKU_MISMATCH",
    );
    assert!(
        plan_item_repo::for_revision(&conn, &w.scope, w.tenant, w.revision.id)
            .await
            .unwrap()
            .is_empty()
    );
    plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry))
        .await
        .unwrap();
}
/// The insert honours a revision's lock on its own, not only its pending state (D-407): a
/// revision naming a pending unit refuses an item even while its state still reads draft. No
/// repository writes that shape (the lock writes both); it is set here to isolate the lock half.
#[tokio::test]
async fn item_insert_honours_the_lock_without_the_pending_state() {
    use sea_orm::{ColumnTrait, EntityTrait, sea_query::Expr};
    use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    plan_revision::Entity::update_many()
        .secure()
        .scope_with(&w.scope)
        .col_expr(plan_revision::Column::PendingUnitId, Expr::value(Some(u)))
        .filter(sea_orm::Condition::all().add(plan_revision::Column::Id.eq(w.revision.id)))
        .exec(&conn)
        .await
        .unwrap();
    let locked = plan_revision::Entity::find_by_id(w.revision.id)
        .secure()
        .scope_with(&w.scope)
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (locked.state.as_str(), locked.pending_unit_id),
        ("draft", Some(u))
    );
    conflict(
        plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry)).await,
        "REVISION_NOT_DRAFT",
    );
}
/// The reconciliation scan: confirmed and lost items of every revision state, in id order after
/// the cursor, bounded, across tenants for the trusted ticker.
#[tokio::test]
async fn item_reconcile_batch_is_ordered_bounded_and_skips_unsettled_references() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let mut ids = Vec::new();
    for state in [
        "confirmed",
        "lost",
        "unreserved",
        "confirmation_pending",
        "confirmed",
    ] {
        let mut m = item(&w.revision, &w.entry);
        m.id = Uuid::now_v7();
        m.sku_id = Uuid::new_v4();
        m.price_book_entry_id = None;
        m.treatment = "included".into();
        m.reference_state = state.into();
        m.reservation_id = (state != "unreserved").then(Uuid::new_v4);
        plan_item_repo::insert_as_given(&conn, &w.scope, m.clone())
            .await
            .unwrap();
        if matches!(state, "confirmed" | "lost") {
            ids.push(m.id);
        }
    }
    let all = AccessScope::allow_all();
    let first = plan_item_repo::reconcile_batch(&conn, &all, None, 2)
        .await
        .unwrap();
    assert_eq!(first.iter().map(|m| m.id).collect::<Vec<_>>(), ids[..2]);
    let rest = plan_item_repo::reconcile_batch(&conn, &all, Some(first[1].id), 10)
        .await
        .unwrap();
    assert_eq!(rest.iter().map(|m| m.id).collect::<Vec<_>>(), ids[2..]);
    assert!(
        plan_item_repo::reconcile_batch(&conn, &AccessScope::for_tenant(Uuid::new_v4()), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "a tenant scope sees only its own"
    );
}
/// D-467 (the phase 9 review's theme G, R12, R14, R16, R23): the repository's writers store D-467's
/// row shape whatever the model they are given carries: `plan::stored_treatment` of its entry
/// (`paid`, or `included` for an item without one) and no quantity. A caller that passes a legacy
/// treatment and quantities, to the insert or to the draft update, still stores `paid`, NULL and
/// NULL; the stored row and the written answer agree.
#[tokio::test]
async fn the_writers_store_d467s_row_shape_whatever_the_model_carries() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let shape = |m: &plan_item::Model| (m.treatment.clone(), m.included_qty.clone(), m.qty_min);
    let paid = ("paid".to_owned(), None, None);
    let mut legacy = item(&w.revision, &w.entry);
    legacy.treatment = "optional".into();
    legacy.included_qty = Some("5".into());
    legacy.qty_min = Some(2);
    let written = plan_item_repo::insert(&conn, &w.scope, legacy.clone())
        .await
        .unwrap();
    assert_eq!(shape(&written), paid, "the insert answers what it stored");
    let stored = plan_item_repo::find(&conn, &w.scope, w.tenant, legacy.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shape(&stored), paid, "the insert stores the shape");
    let mut edited = stored.clone();
    edited.treatment = "included".into();
    edited.included_qty = Some("7".into());
    edited.qty_min = Some(1);
    plan_item_repo::update_draft(&conn, &w.scope, edited)
        .await
        .unwrap();
    let stored = plan_item_repo::find(&conn, &w.scope, w.tenant, legacy.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (shape(&stored), stored.version),
        (paid, 2),
        "the update too"
    );
    // An item without an entry is the one entry-less row the CHECK admits: included, no quantity.
    let mut entryless = item(&w.revision, &w.entry);
    entryless.sku_id = Uuid::new_v4();
    entryless.price_book_entry_id = None;
    entryless.treatment = "paid".into();
    entryless.included_qty = Some("3".into());
    let written = plan_item_repo::insert(&conn, &w.scope, entryless)
        .await
        .unwrap();
    assert_eq!(shape(&written), ("included".to_owned(), None, None));
}

#[tokio::test]
async fn item_round_trips_one_per_sku_and_its_columns_are_checked() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let mut included = item(&w.revision, &w.entry);
    included.treatment = "included".into();
    included.included_qty = Some("100.50".into());
    included.qty_min = None;
    let got = plan_item_repo::insert_as_given(&conn, &w.scope, included.clone())
        .await
        .unwrap();
    assert_eq!(got, included);
    assert_eq!(got.included_qty.as_deref(), Some("100.50"), "exact text");
    conflict(
        plan_item_repo::insert_as_given(
            &conn,
            &w.scope,
            plan_item::Model {
                id: Uuid::new_v4(),
                ..included.clone()
            },
        )
        .await,
        "ITEM_SKU_TAKEN",
    );
    // An included item may name no entry; a paid or optional one may not.
    let mut free = item(&w.revision, &w.entry);
    free.sku_id = Uuid::new_v4();
    free.price_book_entry_id = None;
    free.treatment = "included".into();
    plan_item_repo::insert_as_given(&conn, &w.scope, free.clone())
        .await
        .unwrap();
    for treatment in ["paid", "optional"] {
        let mut priced = free.clone();
        priced.id = Uuid::new_v4();
        priced.sku_id = Uuid::new_v4();
        priced.treatment = treatment.into();
        assert!(
            plan_item_repo::insert_as_given(&conn, &w.scope, priced)
                .await
                .is_err(),
            "{treatment} without an entry"
        );
    }
    let bad = |f: &dyn Fn(&mut plan_item::Model)| {
        let mut m = free.clone();
        m.id = Uuid::new_v4();
        m.sku_id = Uuid::new_v4();
        f(&mut m);
        m
    };
    for (what, m) in [
        ("treatment", bad(&|m| m.treatment = "free".into())),
        (
            "reference state",
            bad(&|m| m.reference_state = "held".into()),
        ),
        ("negative qty_min", bad(&|m| m.qty_min = Some(-1))),
        (
            "signed quantity",
            bad(&|m| m.included_qty = Some("-1".into())),
        ),
        ("exponent", bad(&|m| m.included_qty = Some("1e5".into()))),
        ("trailing dot", bad(&|m| m.included_qty = Some("1.".into()))),
        ("empty", bad(&|m| m.included_qty = Some(String::new()))),
    ] {
        assert!(
            plan_item_repo::insert_as_given(&conn, &w.scope, m)
                .await
                .is_err(),
            "the columns must refuse a bad {what}"
        );
    }
    assert_eq!(
        plan_item_repo::for_revision(&conn, &w.scope, w.tenant, w.revision.id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        plan_item_repo::names_entry(&conn, &w.scope, w.tenant, w.entry.id)
            .await
            .unwrap()
    );
    assert!(
        !plan_item_repo::names_entry(&conn, &w.scope, w.tenant, Uuid::new_v4())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn item_draft_edit_is_a_conditional_write_while_its_revision_is_a_draft() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let i = plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry))
        .await
        .unwrap();
    let mut edited = i.clone();
    edited.treatment = "optional".into();
    edited.qty_min = Some(0);
    let scope = w.scope.clone();
    let race = edited.clone();
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            let race = race.clone();
            Box::pin(async move { plan_item_repo::update_draft_as_given(tx, &scope, race).await })
        }),
    )
    .await;
    let got = plan_item_repo::find(&conn, &w.scope, w.tenant, i.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (got.treatment.as_str(), got.qty_min, got.version),
        ("optional", Some(0), 2)
    );
    let mut ghost = got.clone();
    ghost.price_book_entry_id = Some(Uuid::new_v4());
    conflict(
        plan_item_repo::update_draft_as_given(&conn, &w.scope, ghost).await,
        "ENTRY_NOT_FOUND",
    );
    // Once the revision is locked, its items are fixed.
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 1)
            .await
            .unwrap()
    );
    conflict(
        plan_item_repo::update_draft_as_given(&conn, &w.scope, got.clone()).await,
        "STALE_REVISION",
    );
    conflict(
        plan_item_repo::delete_draft(&conn, &w.scope, w.tenant, i.id, got.version).await,
        "STALE_REVISION",
    );
}

#[tokio::test]
async fn item_reference_moves_in_any_revision_state_at_the_observed_version() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let i = plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry))
        .await
        .unwrap();
    // A published revision's item still takes its reference receipt (D-413's attach).
    let u = unit(&w.db, &w.scope, w.tenant, "plan_revision").await;
    assert!(
        plan_revision_repo::try_lock(&conn, &w.scope, w.tenant, w.revision.id, u, 1)
            .await
            .unwrap()
    );
    plan_revision_repo::publish(&conn, &w.scope, w.tenant, w.revision.id, u, at(10))
        .await
        .unwrap();
    let receipt = Uuid::new_v4();
    let (tenant, id, scope) = (w.tenant, i.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                plan_item_repo::set_reference(
                    tx,
                    &scope,
                    tenant,
                    id,
                    1,
                    plan::ReferenceState::Confirmed,
                    Some(receipt),
                    at(11),
                )
                .await
            })
        }),
    )
    .await;
    let got = plan_item_repo::find(&conn, &w.scope, w.tenant, i.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            got.reference_state.as_str(),
            got.reservation_id,
            got.version
        ),
        ("confirmed", Some(receipt), 2)
    );
}

#[tokio::test]
async fn item_delete_is_a_conditional_write() {
    let w = world().await;
    let conn = w.db.conn().unwrap();
    let i = plan_item_repo::insert_as_given(&conn, &w.scope, item(&w.revision, &w.entry))
        .await
        .unwrap();
    let (tenant, id, scope) = (w.tenant, i.id, w.scope.clone());
    exactly_one_wins(
        &w.db,
        &w.dsn,
        "STALE_REVISION",
        Arc::new(move |tx| {
            let scope = scope.clone();
            Box::pin(async move { plan_item_repo::delete_draft(tx, &scope, tenant, id, 1).await })
        }),
    )
    .await;
    assert!(
        plan_item_repo::find(&conn, &w.scope, w.tenant, i.id)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------- conflict vocabulary

#[test]
fn phase_3_unique_messages_match_both_engines() {
    use bss_pricing::infra::storage::repo::unique_code;
    for (message, code) in [
        (
            "duplicate key value violates unique constraint \"pricing_plan_code\"",
            "PLAN_CODE_TAKEN",
        ),
        (
            "UNIQUE constraint failed: pricing_plan.tenant_id, pricing_plan.code",
            "PLAN_CODE_TAKEN",
        ),
        (
            "duplicate key value violates unique constraint \"pricing_plan_revision_no\"",
            "REVISION_NO_TAKEN",
        ),
        (
            "UNIQUE constraint failed: pricing_plan_revision.plan_id, pricing_plan_revision.rev_no",
            "REVISION_NO_TAKEN",
        ),
        (
            "duplicate key value violates unique constraint \"pricing_plan_revision_open\"",
            "REVISION_DRAFT_EXISTS",
        ),
        (
            "duplicate key value violates unique constraint \"pricing_plan_revision_published\"",
            "REVISION_PUBLISHED_EXISTS",
        ),
        (
            "duplicate key value violates unique constraint \"pricing_plan_revision_scheduled\"",
            "REVISION_SCHEDULED_EXISTS",
        ),
        (
            "duplicate key value violates unique constraint \"pricing_plan_item_sku\"",
            "ITEM_SKU_TAKEN",
        ),
        (
            "UNIQUE constraint failed: pricing_plan_item.revision_id, pricing_plan_item.sku_id",
            "ITEM_SKU_TAKEN",
        ),
    ] {
        assert_eq!(unique_code(message), Some(code), "{message}");
    }
    // SQLite names only the columns of a partial index: the three single-column revision indexes
    // read alike, so the shared matcher leaves them to the revision repository, which knows the
    // state it wrote.
    assert_eq!(
        unique_code("UNIQUE constraint failed: pricing_plan_revision.plan_id"),
        None
    );
}
