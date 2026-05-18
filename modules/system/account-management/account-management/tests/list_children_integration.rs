//! Real-DB integration tests for `TenantRepoImpl::list_children`
//! exercising the `OData` filter / cursor surface against in-memory
//! `SQLite`.
//!
//! Unit-level coverage lives in
//! `domain::tenant::service::service_tests` and pins service-layer
//! semantics (PEP gate, parent guard, lifter shape) against the
//! `FakeTenantRepo` — that fake honours only the minimal
//! `$filter=status eq <i16>` predicate it needs to assert the
//! hidden-AND default and the explicit override. The repo-level
//! `paginate_odata` machinery (filter AST → `SeaORM` condition,
//! cursor encode / decode, tiebreaker ordering, base-condition
//! composition) is too easy to misroute on the fake; this file lifts
//! those invariants to the real `SeaORM` path:
//!
//! * Hidden-AND default — empty `$filter` excludes `Deleted`.
//! * Explicit `$filter=status eq 'deleted'` returns Deleted rows.
//! * `$filter=tenant_type_uuid eq <uuid>` partitions a mixed-type
//!   sibling set.
//! * `$filter=self_managed eq true` isolates the barrier children.
//! * Cursor round-trip — `limit=2` over 5 active siblings walks the
//!   full set across three page reads, observing exactly the inputs
//!   the first page suggests.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use account_management::domain::tenant::TenantRepo;
use account_management_sdk::TenantInfoFilterField;
use modkit_odata::ast::{CompareOperator, Expr, Value as OdataValue};
use modkit_odata::filter::FilterField;
use modkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use sea_orm::ActiveValue;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use account_management::infra::storage::entity::tenants;
use common::*;

// ---- helpers ---------------------------------------------------------

const ROOT_ID: u128 = 0x100;

fn ts_at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + secs).expect("epoch + offset")
}

/// Seed a tenant with explicit `created_at` so `(created_at ASC, id ASC)`
/// ordering stays deterministic across the cursor round-trip. The
/// shared `common::insert_tenant` helper uses `now_utc()` which can
/// collide at microsecond precision on fast sequential inserts.
async fn seed_tenant_at(
    h: &Harness,
    id: Uuid,
    parent_id: Uuid,
    status_smallint: i16,
    self_managed: bool,
    tenant_type_uuid: Uuid,
    created_at: OffsetDateTime,
) {
    use modkit_db::secure::secure_insert;
    let conn = h.provider.conn().expect("conn");
    let am = tenants::ActiveModel {
        id: ActiveValue::Set(id),
        parent_id: ActiveValue::Set(Some(parent_id)),
        name: ActiveValue::Set(format!("t-{id}")),
        status: ActiveValue::Set(status_smallint),
        self_managed: ActiveValue::Set(self_managed),
        tenant_type_uuid: ActiveValue::Set(tenant_type_uuid),
        depth: ActiveValue::Set(1),
        created_at: ActiveValue::Set(created_at),
        updated_at: ActiveValue::Set(created_at),
        deleted_at: ActiveValue::Set(if status_smallint == DELETED {
            Some(created_at)
        } else {
            None
        }),
        retention_window_secs: ActiveValue::Set(None),
        claimed_by: ActiveValue::Set(None),
        claimed_at: ActiveValue::Set(None),
        terminal_failure_at: ActiveValue::Set(None),
    };
    secure_insert::<tenants::Entity>(am, &allow_all(), &conn)
        .await
        .expect("seed child");
}

async fn seed_root(h: &Harness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("root self-row");
}

/// Build `$filter=<field> eq <i16>` for a numeric column. Used by
/// non-status numeric columns in these tests (none currently — kept
/// as scaffold for future numeric filters).
#[allow(dead_code)]
fn filter_field_eq_i64(field: &str, value: i64) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(field.to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::Number(value.into()))),
    )
}

/// Build `$filter=<field> eq '<label>'` for a string-encoded column
/// (e.g. the `status` lifecycle enum exposed as a string contract on
/// the SDK surface).
fn filter_field_eq_string(field: &str, label: &str) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(field.to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::String(label.to_owned()))),
    )
}

fn filter_field_eq_uuid(field: &str, value: Uuid) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(field.to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::Uuid(value))),
    )
}

fn filter_field_eq_bool(field: &str, value: bool) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(field.to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::Bool(value))),
    )
}

fn ids_of(items: &[account_management::domain::tenant::model::TenantModel]) -> Vec<Uuid> {
    items.iter().map(|t| t.id).collect()
}

// ---- pinned filter-field surface ------------------------------------

/// Sanity-pin against the SDK's declared filter set — duplicated from
/// the SDK-side `tenant_info_filter_fields_are_pinned` so the
/// integration suite trips immediately on a wire-contract drift even
/// when the SDK lib tests are not re-run.
#[test]
fn tenant_info_filter_fields_are_stable() {
    let names: Vec<&'static str> = TenantInfoFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec![
            "id",
            "status",
            "tenant_type_uuid",
            "self_managed",
            "created_at",
            "updated_at",
        ],
    );
}

// ---- hidden-AND status default --------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_default_filter_hides_deleted() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let active = Uuid::from_u128(0x201);
    let suspended = Uuid::from_u128(0x202);
    let deleted = Uuid::from_u128(0x203);
    let type_a = Uuid::from_u128(0xAA);
    seed_tenant_at(&h, active, root, ACTIVE, false, type_a, ts_at(1)).await;
    seed_tenant_at(&h, suspended, root, SUSPENDED, false, type_a, ts_at(2)).await;
    seed_tenant_at(&h, deleted, root, DELETED, false, type_a, ts_at(3)).await;

    let page = h
        .repo
        .list_children(&allow_all(), root, &ODataQuery::default())
        .await
        .expect("list");

    assert_eq!(
        ids_of(&page.items),
        vec![active, suspended],
        "empty `$filter` MUST drop Deleted rows via the repo-level \
         hidden-AND default"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_explicit_status_filter_returns_deleted() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let active = Uuid::from_u128(0x201);
    let deleted = Uuid::from_u128(0x202);
    let type_a = Uuid::from_u128(0xAA);
    seed_tenant_at(&h, active, root, ACTIVE, false, type_a, ts_at(1)).await;
    seed_tenant_at(&h, deleted, root, DELETED, false, type_a, ts_at(2)).await;

    let query = ODataQuery::default().with_filter(filter_field_eq_string("status", "deleted"));
    let page = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect("list");

    assert_eq!(
        ids_of(&page.items),
        vec![deleted],
        "`$filter=status eq 'deleted'` MUST bypass the hidden-AND \
         default and return the soft-deleted row"
    );
}

/// `status` filter values outside the public SDK contract — including
/// the AM-internal `'provisioning'` — surface as a validation error
/// from `TenantODataMapper::map_value` before the predicate reaches
/// `SeaORM`. The storage SMALLINT encoding is intentionally NOT part
/// of the wire contract, so numeric forms (`status eq 3`) are also
/// rejected by the framework's kind-validation step before this hook
/// even runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_rejects_unknown_status_value() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let query = ODataQuery::default().with_filter(filter_field_eq_string("status", "wat"));
    let err = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect_err("unknown status value MUST be rejected");
    let detail = format!("{err:?}");
    assert!(
        detail.contains("invalid `status` value") || detail.contains("invalid status"),
        "expected validation error referencing the invalid status; got {detail}"
    );

    // `'provisioning'` is the AM-internal status and has no SDK
    // representation; the map_value hook MUST reject it the same way
    // as any other unknown string.
    let query = ODataQuery::default().with_filter(filter_field_eq_string("status", "provisioning"));
    let err = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect_err("'provisioning' MUST be rejected (internal status)");
    let detail = format!("{err:?}");
    assert!(
        detail.contains("invalid `status` value"),
        "expected validation error referencing the invalid status; got {detail}"
    );
}

/// `status` is a categorical lifecycle column. Ordered operators
/// (`lt`/`le`/`gt`/`ge`) on the wire string would silently fall
/// back to the hidden storage ordinal (`status < 3` meaning "any
/// SMALLINT less than `Deleted`"), which is a confusing semantic
/// mismatch with the public string contract. The AM mapper rejects
/// these operators via `FieldToColumn::map_value`; the framework
/// surfaces the rejection as a validation error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_rejects_ordered_comparison_on_status() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let lt = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Lt,
        Box::new(Expr::Value(OdataValue::String("deleted".to_owned()))),
    );
    let query = ODataQuery::default().with_filter(lt);
    let err = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect_err("ordered comparison on status MUST be rejected");
    let detail = format!("{err:?}");
    assert!(
        detail.contains("not supported on `status`") || detail.contains("ordered"),
        "expected mapper rejection for ordered operator; got {detail}"
    );
}

/// `$orderby=status` is rejected by the framework before the
/// effective-order is composed: the column is exposed as a string on
/// the wire but is `SMALLINT` in storage, so the cursor codec would
/// either fail to decode the token (decoded as a String when the
/// model side returned a `SmallInt`) or compare against the wrong
/// SQL type on the next page. `TenantODataMapper::is_orderable`
/// returns `false` for `Status`, which the framework's order-
/// validation loop trips on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_rejects_orderby_on_status() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let query = ODataQuery::default().with_order(ODataOrderBy(vec![OrderKey {
        field: "status".to_owned(),
        dir: SortDir::Asc,
    }]));
    let err = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect_err("$orderby=status MUST be rejected");
    let detail = format!("{err:?}");
    assert!(
        detail.contains("status") || detail.contains("InvalidOrderByField"),
        "expected order-validation rejection; got {detail}"
    );
}

// ---- typed filters --------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_filter_tenant_type_uuid_partitions_set() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let type_a = Uuid::from_u128(0xAA);
    let type_b = Uuid::from_u128(0xBB);
    let a1 = Uuid::from_u128(0x301);
    let a2 = Uuid::from_u128(0x302);
    let b1 = Uuid::from_u128(0x303);
    let b2 = Uuid::from_u128(0x304);
    seed_tenant_at(&h, a1, root, ACTIVE, false, type_a, ts_at(1)).await;
    seed_tenant_at(&h, a2, root, ACTIVE, false, type_a, ts_at(2)).await;
    seed_tenant_at(&h, b1, root, ACTIVE, false, type_b, ts_at(3)).await;
    seed_tenant_at(&h, b2, root, ACTIVE, false, type_b, ts_at(4)).await;

    let query = ODataQuery::default().with_filter(filter_field_eq_uuid("tenant_type_uuid", type_a));
    let page = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect("list");

    assert_eq!(
        ids_of(&page.items),
        vec![a1, a2],
        "`$filter=tenant_type_uuid eq <type-A>` MUST partition the set \
         to type-A rows only"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_filter_self_managed_isolates_barrier_children() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let type_a = Uuid::from_u128(0xAA);
    let plain1 = Uuid::from_u128(0x401);
    let plain2 = Uuid::from_u128(0x402);
    let boundary = Uuid::from_u128(0x403);
    seed_tenant_at(&h, plain1, root, ACTIVE, false, type_a, ts_at(1)).await;
    seed_tenant_at(&h, plain2, root, ACTIVE, false, type_a, ts_at(2)).await;
    seed_tenant_at(&h, boundary, root, ACTIVE, true, type_a, ts_at(3)).await;

    let query = ODataQuery::default().with_filter(filter_field_eq_bool("self_managed", true));
    let page = h
        .repo
        .list_children(&allow_all(), root, &query)
        .await
        .expect("list");

    assert_eq!(
        ids_of(&page.items),
        vec![boundary],
        "`$filter=self_managed eq true` MUST return only the barrier child"
    );
}

// ---- cursor pagination ----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_cursor_pagination_walks_full_set() {
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let type_a = Uuid::from_u128(0xAA);
    // Stamp each child with a strictly-increasing `created_at` so the
    // tiebreaker order (`created_at ASC`) is fully deterministic — at
    // microsecond resolution sequential inserts can collide.
    let ids: Vec<Uuid> = (0..5u128).map(|i| Uuid::from_u128(0x500 + i)).collect();
    for (i, id) in ids.iter().enumerate() {
        seed_tenant_at(
            &h,
            *id,
            root,
            ACTIVE,
            false,
            type_a,
            ts_at(1) + Duration::seconds(i64::try_from(i).unwrap()),
        )
        .await;
    }

    // First page — limit=2, no cursor: returns ids 0..2, must carry
    // `next_cursor` because there are 3 more rows.
    let first = h
        .repo
        .list_children(&allow_all(), root, &ODataQuery::default().with_limit(2))
        .await
        .expect("first");
    assert_eq!(ids_of(&first.items), vec![ids[0], ids[1]]);
    let cursor1 = first
        .page_info
        .next_cursor
        .expect("must yield next_cursor when 5 rows > limit=2");
    assert!(first.page_info.prev_cursor.is_none());

    // Second page — feed the cursor; returns ids 2..4 + next_cursor.
    let cv1 = CursorV1::decode(&cursor1).expect("decode cursor1");
    let second = h
        .repo
        .list_children(
            &allow_all(),
            root,
            &ODataQuery::default().with_limit(2).with_cursor(cv1),
        )
        .await
        .expect("second");
    assert_eq!(ids_of(&second.items), vec![ids[2], ids[3]]);
    let cursor2 = second
        .page_info
        .next_cursor
        .expect("must yield next_cursor when row 4 remains");

    // Third page — single row, no further next_cursor.
    let cv2 = CursorV1::decode(&cursor2).expect("decode cursor2");
    let third = h
        .repo
        .list_children(
            &allow_all(),
            root,
            &ODataQuery::default().with_limit(2).with_cursor(cv2),
        )
        .await
        .expect("third");
    assert_eq!(ids_of(&third.items), vec![ids[4]]);
    assert!(
        third.page_info.next_cursor.is_none(),
        "final page MUST NOT carry next_cursor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_children_cursor_pagination_survives_created_at_collision() {
    // Regression pin for the cursor row-loss bug fixed by switching
    // `paginate_odata`'s tiebreaker from `("created_at", Asc)` (non-
    // unique → effective order is partial) to `("id", Asc)` (PK,
    // total order). When two siblings share a `created_at`
    // timestamp the pre-fix cursor predicate `created_at > last_ts`
    // skipped the collision-mates on the next page.
    //
    // Seed four siblings with **identical** `created_at` (the worst
    // case: 100% collision). Walk the listing with `limit=1`. The
    // composite `(created_at ASC, id ASC)` total order must surface
    // all four rows in deterministic UUID-ascending order across
    // four pages with no row-loss.
    let h = setup_sqlite().await.expect("harness");
    let root = Uuid::from_u128(ROOT_ID);
    seed_root(&h, root).await;

    let type_a = Uuid::from_u128(0xAA);
    // Same `created_at` for every sibling. UUIDs in numeric order so
    // the predicted text-collation order (`00000000-...-000000000600`,
    // `...0601`, `...0602`, `...0603`) matches numeric order.
    let collision_ts = ts_at(1);
    let ids: Vec<Uuid> = (0..4u128).map(|i| Uuid::from_u128(0x600 + i)).collect();
    for id in &ids {
        seed_tenant_at(&h, *id, root, ACTIVE, false, type_a, collision_ts).await;
    }

    // Walk all four pages with limit=1; collect ids returned.
    let mut walked: Vec<Uuid> = Vec::new();
    let mut cursor: Option<CursorV1> = None;
    for page_n in 0..4 {
        let mut q = ODataQuery::default().with_limit(1);
        if let Some(c) = cursor.take() {
            q = q.with_cursor(c);
        }
        let page = h
            .repo
            .list_children(&allow_all(), root, &q)
            .await
            .expect("paged list");
        assert_eq!(
            page.items.len(),
            1,
            "page {page_n} MUST return exactly one row (limit=1, four rows total)"
        );
        walked.push(page.items[0].id);
        cursor = page
            .page_info
            .next_cursor
            .and_then(|s| CursorV1::decode(&s).ok());
    }

    // Total order guarantee: union of walked == seeded set, in
    // deterministic UUID-ascending order. Without the fix the second
    // page would return 0 rows (filter `created_at > last_ts`
    // matches nothing under collision) and `walked.len()` would
    // be 1 — the assertion above would already trip.
    assert_eq!(
        walked, ids,
        "cursor walk over `(created_at ASC, id ASC)` MUST surface every \
         row in UUID-ascending order when `created_at` is identical \
         across siblings"
    );
}
