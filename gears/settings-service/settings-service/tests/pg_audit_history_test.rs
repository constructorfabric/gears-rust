#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    reason = "the fixture and the seed are plain helpers, which \
              `allow-expect-in-tests` does not reach; a failed expectation \
              there is the test failing, as in the sibling PostgreSQL suites"
)]
//! The history of a setting over a tenant's subtree on a real `PostgreSQL`.
//!
//! What `SQLite` cannot show: that the statement is one `PostgreSQL` takes —
//! a tenant list of the whole subtree budget in one `IN`, `uuid` and
//! `timestamptz` keyset values in the cursor predicate — and that its order
//! and its tiebreak are the ones the pages are cut by there, where a `uuid`
//! and a `timestamptz` compare by the backend's own rules rather than as the
//! text `SQLite` stores.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run with:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_audit_history_test
//! ```

use std::sync::Arc;

use cf_gears_settings_service::audit::{
    ActorTenant, AuditOperation, AuditRecord, AuditSink, StoredAuditRecord,
};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::domain::ports::NoMetrics;
use cf_gears_settings_service::domain::resolution::SUBTREE_BUDGET;
use cf_gears_settings_service::infra::storage::audit_store::AuditStore;
use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::{CursorV1, ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

mod common;
use common::{PgFixture, pg_fixture};

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~";

/// A migrated container and the provider the store reads through.
async fn migrated() -> Option<(PgFixture, DBProvider<DbError>)> {
    let fixture = pg_fixture("audit history").await?;
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    let db = DBProvider::<DbError>::new(fixture.db.clone());
    Some((fixture, db))
}

fn record(tenant: Option<Uuid>, request: &str) -> AuditRecord {
    AuditRecord::new(
        KEY,
        tenant,
        "admin",
        ActorTenant::machine(),
        AuditOperation::Change,
        request,
    )
}

/// Append one record and let the microsecond clock move on.
///
/// Records written back to back can land in the same microsecond, and the id,
/// which is random, then decides their order. A test about "newest first"
/// must not depend on that: the pause keeps every appended record on an
/// instant of its own. The tie itself is covered by
/// `records_of_one_instant_are_ordered_and_paged_by_id`.
async fn append(db: &DBProvider<DbError>, record: AuditRecord) {
    let conn = db.conn().expect("connection");
    AuditStore::new(Arc::new(NoMetrics))
        .append(&conn, &AccessScope::allow_all(), record)
        .await
        .expect("append");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

fn query(limit: Option<u64>, cursor: Option<CursorV1>) -> ODataQuery {
    ODataQuery {
        limit,
        cursor,
        ..ODataQuery::default()
    }
}

async fn try_subtree(
    db: &DBProvider<DbError>,
    target: Uuid,
    descendants: &[Uuid],
    limit: Option<u64>,
    cursor: Option<CursorV1>,
) -> Result<Page<StoredAuditRecord>, DomainError> {
    let conn = db.conn().expect("connection");
    AuditStore::history_subtree(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        descendants,
        &query(limit, cursor),
    )
    .await
}

async fn subtree(
    db: &DBProvider<DbError>,
    target: Uuid,
    descendants: &[Uuid],
    limit: Option<u64>,
    cursor: Option<CursorV1>,
) -> Page<StoredAuditRecord> {
    try_subtree(db, target, descendants, limit, cursor)
        .await
        .expect("the subtree history reads")
}

fn requests(page: &Page<StoredAuditRecord>) -> Vec<&str> {
    page.items.iter().map(|r| r.request_id.as_str()).collect()
}

fn next_of(page: &Page<StoredAuditRecord>) -> Option<CursorV1> {
    page.page_info
        .next_cursor
        .as_deref()
        .map(|c| CursorV1::decode(c).expect("cursor decodes"))
}

/// Every page of a walk in pages of `limit`, as the ids it listed.
async fn walk(
    db: &DBProvider<DbError>,
    target: Uuid,
    descendants: &[Uuid],
    limit: u64,
) -> Vec<Uuid> {
    let mut walked = Vec::new();
    let mut cursor = None;
    for _ in 0..1_000 {
        let page = subtree(db, target, descendants, Some(limit), cursor).await;
        walked.extend(page.items.iter().map(|r| r.id));
        cursor = next_of(&page);
        if cursor.is_none() {
            return walked;
        }
    }
    panic!("the walk did not end");
}

#[tokio::test]
async fn a_subtree_history_holds_the_listed_tenants_and_the_definition_newest_first() {
    let Some((_fixture, db)) = migrated().await else {
        return;
    };
    let (target, child, grandchild, outside) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    append(&db, record(None, "definition")).await;
    for (tenant, request) in [
        (target, "t1"),
        (child, "c1"),
        (outside, "o1"),
        (grandchild, "g1"),
        (target, "t2"),
        (outside, "o2"),
        (child, "c2"),
    ] {
        append(&db, record(Some(tenant), request)).await;
    }

    let page = subtree(&db, target, &[child, grandchild], None, None).await;
    assert_eq!(
        requests(&page),
        vec!["c2", "t2", "g1", "c1", "t1", "definition"],
        "the target, the listed descendants and the definition, newest first across them"
    );
    let alone = subtree(&db, target, &[], None, None).await;
    assert_eq!(requests(&alone), vec!["t2", "t1", "definition"]);

    // Paged, the same list in the same order, each record once.
    let whole: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    assert_eq!(walk(&db, target, &[child, grandchild], 2).await, whole);
}

#[tokio::test]
async fn records_of_one_instant_are_ordered_and_paged_by_id() {
    // Rows of two tenants with one `occurred_at`: the id decides, as a `uuid`
    // compares on this backend, and a page boundary inside the tie neither
    // repeats nor skips a record.
    let Some((fixture, db)) = migrated().await else {
        return;
    };
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    let mut ids: Vec<Uuid> = (0..7).map(|_| Uuid::new_v4()).collect();
    for (i, id) in ids.iter().enumerate() {
        let tenant = if i % 2 == 0 { target } else { child };
        fixture
            .conn
            .execute_unprepared(&format!(
                "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, \
                 actor, actor_classification, outcome, request_id, occurred_at) \
                 VALUES ('{id}', 'cf.settings:{KEY}@{tenant}', '{KEY}', '{tenant}', 'change', \
                 'admin', 'public', 'success', 'tie-{i}', \
                 TIMESTAMPTZ '2026-01-01 00:00:00.123456+00')"
            ))
            .await
            .expect("the row is inserted");
    }
    ids.sort();
    ids.reverse();

    let whole = subtree(&db, target, &[child], None, None).await;
    let listed: Vec<Uuid> = whole.items.iter().map(|r| r.id).collect();
    assert_eq!(
        listed, ids,
        "a tie on the instant is broken by id, descending"
    );
    assert_eq!(walk(&db, target, &[child], 2).await, ids);
    assert_eq!(walk(&db, target, &[child], 3).await, ids);
}

#[tokio::test]
async fn a_cursor_of_one_mode_or_target_is_refused_by_the_other() {
    let Some((_fixture, db)) = migrated().await else {
        return;
    };
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    for (tenant, request) in [(target, "t1"), (child, "c1"), (target, "t2"), (child, "c2")] {
        append(&db, record(Some(tenant), request)).await;
    }
    let conn = db.conn().expect("connection");
    let scope_first = AuditStore::history(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        &query(Some(1), None),
    )
    .await
    .expect("the scope history reads");
    assert_eq!(scope_first.total_count, 2, "the scope history is counted");
    let scope_cursor = CursorV1::decode(
        scope_first
            .page_info
            .next_cursor
            .as_deref()
            .expect("a second page of the scope history"),
    )
    .expect("cursor decodes");
    let subtree_first = subtree(&db, target, &[child], Some(1), None).await;
    let subtree_cursor = next_of(&subtree_first).expect("a second page of the subtree history");

    // Each continues its own.
    let continued = subtree(&db, target, &[child], Some(1), Some(subtree_cursor.clone())).await;
    assert_eq!(requests(&continued), vec!["t2"]);
    let continued = AuditStore::history(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        &query(Some(1), Some(scope_cursor.clone())),
    )
    .await
    .expect("the scope history continues");
    assert_eq!(continued.items.len(), 1);

    // And each refuses the other's, and another target's.
    let crossed = try_subtree(&db, target, &[child], Some(1), Some(scope_cursor)).await;
    assert!(
        matches!(crossed, Err(DomainError::Validation { .. })),
        "{crossed:?}"
    );
    let crossed = AuditStore::history(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        &query(Some(1), Some(subtree_cursor.clone())),
    )
    .await;
    assert!(
        matches!(crossed, Err(DomainError::Validation { .. })),
        "{crossed:?}"
    );
    let foreign = try_subtree(&db, child, &[], Some(1), Some(subtree_cursor)).await;
    assert!(
        matches!(foreign, Err(DomainError::Validation { .. })),
        "{foreign:?}"
    );
}

#[tokio::test]
async fn a_subtree_at_the_budget_fits_one_statement() {
    // The target and the whole budget of descendants in one `IN` list, and the
    // cursor's keyset values beside them: under the 65 535 bind parameters the
    // wire protocol takes, so the list is not chunked.
    let Some((_fixture, db)) = migrated().await else {
        return;
    };
    let target = Uuid::new_v4();
    let descendants: Vec<Uuid> = (0..SUBTREE_BUDGET).map(|_| Uuid::new_v4()).collect();
    append(&db, record(Some(target), "at-the-target")).await;
    append(&db, record(Some(descendants[0]), "first")).await;
    append(
        &db,
        record(Some(descendants[descendants.len() - 1]), "last"),
    )
    .await;
    append(&db, record(Some(Uuid::new_v4()), "outside")).await;

    let first = subtree(&db, target, &descendants, Some(2), None).await;
    assert_eq!(requests(&first), vec!["last", "first"]);
    let second = subtree(&db, target, &descendants, Some(2), next_of(&first)).await;
    assert_eq!(requests(&second), vec!["at-the-target"]);
}
