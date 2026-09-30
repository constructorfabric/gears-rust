//! Repository read and write primitives (T4), against the real migrated schema.
//!
//! Every method takes `runner: &impl DBRunner`, so one body serves both a pooled
//! connection and a transaction; the last test exercises the transaction path.
//!
//! Covers GTS-filtered listing and bounded recursive dependency closure.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;
use types_registry::domain::ports::ListFilter;

use gts::GtsIdPattern;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::{ScopeError, SecureEntityExt};
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{TestDir, allow_all, test_db, test_db_file};
use types_registry::domain::admission::Precondition;
use types_registry::domain::admission::fingerprint::{RequestFingerprint, ScopeHash};
use types_registry::domain::enums::{
    DependencyKind, EntityKind, LifecycleStatus, OperationItemStatus, OperationKind,
    OwnershipScope, Plane,
};
use types_registry::domain::ports::{ItemSuccess, NewEntity, NewOperation, NewOperationItem};
use types_registry::infra::storage::entity::dependency;
use types_registry::infra::storage::repo::{
    DependencyRepo, EntityRepo, OperationRepo, PageRequest, VersionFamilyRepo,
};

const NOW: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);
const FAMILY_KEY: &str = "gts.acme.crm.customer.type";
const CUSTOMER_V1: &str = gts_id!("acme.crm.customer.type.v1~");
/// Chained identifiers derived from [`CUSTOMER_V1`]. Every segment of a chain is
/// a full `vendor.package.namespace.type.vMAJOR`, so a shorthand like
/// `…v1~x.a.type.v1~` does not parse — and an unparsable stored identifier can
/// never match a pattern, which would make a list test pass for the wrong reason.
const CUSTOMER_V1_DERIVED_A: &str = gts_id!("acme.crm.customer.type.v1~acme.crm.a.type.v1~");
const CUSTOMER_V1_DERIVED_B: &str = gts_id!("acme.crm.customer.type.v1~acme.crm.b.type.v1~");

type Provider = Arc<DBProvider<DbError>>;

fn new_entity(gts_id: &str, family_id: i64) -> NewEntity {
    NewEntity {
        gts_uuid: Uuid::new_v5(&Uuid::NAMESPACE_URL, gts_id.as_bytes()),
        gts_id: gts_id.to_owned(),
        entity_kind: EntityKind::TypeSchema,
        family_id,
        ownership_scope: OwnershipScope::Global,
        owner_tenant_id: None,
        owning_gear: Some("types-registry".to_owned()),
        now: NOW,
    }
}

/// Seed one global family plus the given identifiers; returns the family id.
async fn seed(db: &Provider, ids: &[&str]) -> i64 {
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let (family, _) = VersionFamilyRepo::create_or_get(
        &conn,
        &scope,
        FAMILY_KEY,
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("family");
    for id in ids {
        EntityRepo::insert(&conn, &scope, new_entity(id, family.id))
            .await
            .unwrap_or_else(|e| panic!("insert {id}: {e}"));
    }
    family.id
}

async fn entity_id(db: &Provider, gts_id: &str) -> i64 {
    let conn = db.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .expect("row")
        .id
}

// ---------------------------------------------------------------------------
// version_family — insert-if-absent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn family_create_or_get_creates_once_then_reads() {
    let db = test_db().await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();

    let (first, created) = VersionFamilyRepo::create_or_get(
        &conn,
        &scope,
        FAMILY_KEY,
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("first create");
    assert!(created, "the first caller creates the family");

    let (second, again) = VersionFamilyRepo::create_or_get(
        &conn,
        &scope,
        FAMILY_KEY,
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("second call");
    assert!(!again, "the second caller reads the winner's row");
    assert_eq!(
        second.id, first.id,
        "ownership is fixed by the first member"
    );
}

/// What `uq_tr_version_family_key` exists for: whatever the interleaving, there is
/// exactly one row and every caller agrees which one.
///
/// `SQLite` takes a database-wide write lock, so a concurrent writer can see
/// `SQLITE_BUSY`. The test retries that specific case rather than pretending the
/// backend has row-level concurrency it does not have; the assertion is on the end
/// state, which is what the constraint promises.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_family_creation_yields_exactly_one_row() {
    let dir = TestDir::new("tr-repo");
    let db = test_db_file(&dir.path().join("families.db")).await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = db.clone();
        handles.push(tokio::spawn(async move {
            let scope = allow_all();
            for _attempt in 0..50 {
                let conn = db.conn().expect("conn");
                match VersionFamilyRepo::create_or_get(
                    &conn,
                    &scope,
                    FAMILY_KEY,
                    OwnershipScope::Global,
                    None,
                    NOW,
                )
                .await
                {
                    Ok(result) => return result,
                    Err(e) => {
                        // Both contention spellings: `SQLITE_LOCKED` renders
                        // "table is locked", `SQLITE_BUSY` may render "busy"
                        // depending on the driver and the statement.
                        let text = format!("{e}").to_lowercase();
                        if text.contains("locked") || text.contains("busy") {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        } else {
                            panic!("create_or_get failed: {e}");
                        }
                    }
                }
            }
            panic!("gave up waiting for the SQLite write lock");
        }));
    }

    let mut ids = Vec::new();
    let mut creators = 0;
    for handle in handles {
        let (model, created) = handle.await.expect("task");
        ids.push(model.id);
        if created {
            creators += 1;
        }
    }

    assert_eq!(creators, 1, "exactly one caller may create the family");
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "every caller must agree on the family row: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// entity — compare-and-swap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cas_with_the_current_version_advances_it_by_one() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let id = entity_id(&db, CUSTOMER_V1).await;

    assert_eq!(
        EntityRepo::compare_and_swap_version(&conn, &scope, id, 1, NOW)
            .await
            .expect("cas"),
        Some(2),
        "a successful CAS returns the value it wrote"
    );
    let reread = EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(reread.resource_version, 2);
}

#[tokio::test]
async fn cas_with_a_stale_version_affects_no_row_and_reports_it() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let id = entity_id(&db, CUSTOMER_V1).await;

    assert_eq!(
        EntityRepo::compare_and_swap_version(&conn, &scope, id, 1, NOW)
            .await
            .expect("first cas"),
        Some(2)
    );
    let stale = EntityRepo::compare_and_swap_version(&conn, &scope, id, 1, NOW)
        .await
        .expect("a stale CAS reports failure rather than erroring");
    assert_eq!(stale, None, "a stale expected version affects zero rows");

    let reread = EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(reread.resource_version, 2, "the failed CAS changed nothing");
}

#[tokio::test]
async fn cas_refuses_to_advance_past_the_integer_ceiling() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let id = entity_id(&db, CUSTOMER_V1).await;

    let error = EntityRepo::compare_and_swap_version(&conn, &scope, id, i64::MAX, NOW)
        .await
        .expect_err("the next resource version is not representable");
    assert!(matches!(error, ScopeError::Db(_)), "got {error}");

    let delete_error = EntityRepo::mark_deleted(&conn, &scope, id, i64::MAX, NOW)
        .await
        .expect_err("deletion cannot wrap the same resource version");
    assert!(
        matches!(delete_error, ScopeError::Db(_)),
        "got {delete_error}"
    );

    let reread = EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(reread.resource_version, 1, "overflow changed no row");
}

#[tokio::test]
async fn insert_starts_the_resource_version_at_one() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let row = EntityRepo::find_by_gts_id(&conn, &allow_all(), CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.resource_version, 1);
    assert_eq!(row.lifecycle_status, LifecycleStatus::Active);
}

// ---------------------------------------------------------------------------
// entity — pattern list and keyset paging
// ---------------------------------------------------------------------------

/// The pattern is exact in SQL: siblings inside the identifier range, another
/// major and another type never reach the page.
#[tokio::test]
async fn list_returns_exactly_what_the_pattern_accepts() {
    let db = test_db().await;
    seed(
        &db,
        &[
            CUSTOMER_V1,
            gts_id!("acme.crm.customer.type.v2~"),
            gts_id!("acme.crm.customer.other.v1~"),
            gts_id!("acme.crm.invoice.type.v1~"),
        ],
    )
    .await;
    let conn = db.conn().expect("conn");

    let pattern = GtsIdPattern::try_new(CUSTOMER_V1).expect("pattern");
    let page = EntityRepo::list_page(
        &conn,
        &allow_all(),
        &by_pattern(&pattern),
        PageRequest::first(10),
    )
    .await
    .expect("list");

    let ids: Vec<&str> = page.items.iter().map(|m| m.gts_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![CUSTOMER_V1],
        "neither `other.v1~` nor `type.v2~` matches"
    );
}

/// A trailing `~*` covers the derived chain **and the base itself**: a bare
/// segment is an "implicit derived-type coverage" envelope in the GTS spec
/// (§3.6), so `…v1~` and `…v1~*` accept the same set. The point of the test is
/// that the chained identifiers, whose bytes extend past the base, are kept.
#[tokio::test]
async fn list_with_a_trailing_wildcard_returns_the_base_and_its_derived_identifiers() {
    let db = test_db().await;
    seed(
        &db,
        &[
            CUSTOMER_V1,
            CUSTOMER_V1_DERIVED_A,
            CUSTOMER_V1_DERIVED_B,
            gts_id!("acme.crm.invoice.type.v1~"),
        ],
    )
    .await;
    let conn = db.conn().expect("conn");

    let pattern = GtsIdPattern::try_new(gts_id!("acme.crm.customer.type.v1~*")).expect("pattern");
    let page = EntityRepo::list_page(
        &conn,
        &allow_all(),
        &by_pattern(&pattern),
        PageRequest::first(10),
    )
    .await
    .expect("list");
    let ids: Vec<&str> = page.items.iter().map(|m| m.gts_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![CUSTOMER_V1, CUSTOMER_V1_DERIVED_A, CUSTOMER_V1_DERIVED_B],
    );
    assert_eq!(page.next_after, None);
}

#[tokio::test]
async fn list_excludes_deleted_rows() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let id = entity_id(&db, CUSTOMER_V1).await;
    assert_eq!(
        EntityRepo::mark_deleted(&conn, &scope, id, 1, NOW)
            .await
            .expect("delete"),
        Some(2)
    );

    let page = EntityRepo::list_page(
        &conn,
        &scope,
        &ListFilter::default(),
        PageRequest::first(10),
    )
    .await
    .expect("list");
    assert!(
        page.items.is_empty(),
        "a tombstone stays reverse-resolvable by key but leaves discovery"
    );
    assert!(
        EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
            .await
            .expect("keyed read")
            .is_some(),
        "the keyed read still finds the tombstone"
    );
}

/// A tombstone is written once. `mark_deleted` requires the row to be `Active`,
/// so a second deletion reports failure rather than moving `deleted_at` — which
/// is what keeps the tombstone's timestamp meaningful as a purge clock.
///
/// The read-back also pins the enum lowering: `LifecycleStatus::Deleted` goes
/// through `Expr::value` here rather than through an `ActiveModel`, so a wrong
/// smallint would store silently and only surface as a row that never leaves
/// discovery.
#[tokio::test]
async fn a_second_deletion_reports_failure_and_leaves_the_tombstone_alone() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let id = entity_id(&db, CUSTOMER_V1).await;

    assert_eq!(
        EntityRepo::mark_deleted(&conn, &scope, id, 1, NOW)
            .await
            .expect("first delete"),
        Some(2)
    );
    let tombstone = EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(tombstone.lifecycle_status, LifecycleStatus::Deleted);
    assert_eq!(tombstone.deleted_at, Some(NOW));
    assert_eq!(tombstone.resource_version, 2);

    let later = NOW + time::Duration::hours(1);
    assert_eq!(
        EntityRepo::mark_deleted(&conn, &scope, id, 2, later)
            .await
            .expect("a second delete reports failure rather than erroring"),
        None,
        "the row is no longer active, so the CAS must affect zero rows"
    );
    let reread = EntityRepo::find_by_gts_id(&conn, &scope, CUSTOMER_V1)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(reread.deleted_at, Some(NOW), "deleted_at must not move");
    assert_eq!(reread.resource_version, 2, "and neither must the version");
}

#[tokio::test]
async fn keyset_paging_yields_every_row_exactly_once() {
    let db = test_db().await;
    let ids: Vec<String> = (1..=7)
        .map(|i| format!("{}acme.crm.customer.type.v{i}~", gts::GTS_ID_PREFIX))
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    seed(&db, &refs).await;
    let conn = db.conn().expect("conn");

    let mut seen: Vec<String> = Vec::new();
    let mut request = PageRequest::first(3);
    loop {
        let page = EntityRepo::list_page(&conn, &allow_all(), &ListFilter::default(), request)
            .await
            .expect("page");
        assert!(page.items.len() <= 3, "a page never exceeds its limit");
        seen.extend(page.items.iter().map(|m| m.gts_id.clone()));
        let Some(next) = page.next_after else {
            break;
        };
        assert_eq!(page.items.len(), 3, "a page with a continuation is full");
        request = PageRequest::after(next, 3);
    }

    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(seen, expected, "every row exactly once, in gts_id order");
}

/// The keyset boundary is a stored `gts_id`, not an offset, so a row inserted
/// mid-traversal can only land ahead of the cursor or behind it — never shift a
/// row across a page boundary.
#[tokio::test]
async fn a_row_inserted_mid_traversal_neither_duplicates_nor_hides() {
    let db = test_db().await;
    let family_id = seed(
        &db,
        &[
            gts_id!("acme.crm.customer.type.v1~"),
            gts_id!("acme.crm.customer.type.v3~"),
            gts_id!("acme.crm.customer.type.v5~"),
            gts_id!("acme.crm.customer.type.v7~"),
        ],
    )
    .await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();

    let first = EntityRepo::list_page(&conn, &scope, &ListFilter::default(), PageRequest::first(2))
        .await
        .expect("first page");
    assert_eq!(
        first
            .items
            .iter()
            .map(|m| m.gts_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            gts_id!("acme.crm.customer.type.v1~"),
            gts_id!("acme.crm.customer.type.v3~")
        ]
    );
    let cursor = first.next_after.clone().expect("cursor");

    for id in [
        gts_id!("acme.crm.customer.type.v2~"),
        gts_id!("acme.crm.customer.type.v6~"),
    ] {
        EntityRepo::insert(&conn, &scope, new_entity(id, family_id))
            .await
            .expect("insert mid-traversal");
    }

    let second = EntityRepo::list_page(
        &conn,
        &scope,
        &ListFilter::default(),
        PageRequest::after(cursor, 10),
    )
    .await
    .expect("second page");
    let ids: Vec<&str> = second.items.iter().map(|m| m.gts_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            gts_id!("acme.crm.customer.type.v5~"),
            gts_id!("acme.crm.customer.type.v6~"),
            gts_id!("acme.crm.customer.type.v7~"),
        ],
        "the row behind the cursor must not reappear and the one ahead must not hide"
    );
}

/// A sparse pattern costs no extra round trips: the only match, sorted after
/// thousands of rows in its identifier range, arrives on the first page with no
/// continuation.
///
/// `v9~` sorts after every `v2xxx~` in byte order (`'9' > '2'`).
#[tokio::test]
async fn a_sparse_pattern_returns_its_match_on_the_first_page() {
    const MATCH: &str = gts_id!("acme.crm.customer.type.v9~");
    const DECOYS: i32 = 2100;

    let db = test_db().await;
    let family_id = seed(&db, &[MATCH]).await;
    db.transaction(|tx| {
        Box::pin(async move {
            let scope = allow_all();
            for i in 0..DECOYS {
                // Inside `gts.acme.crm.customer.type.`, sorted ahead of the match.
                let id = format!("{}acme.crm.customer.type.v2{i:04}~", gts::GTS_ID_PREFIX);
                EntityRepo::insert(tx, &scope, new_entity(&id, family_id))
                    .await
                    .expect("decoy");
            }
            Ok::<(), DbError>(())
        })
    })
    .await
    .expect("seed decoys");

    let conn = db.conn().expect("conn");
    let pattern = GtsIdPattern::try_new(MATCH).expect("pattern");
    let page = EntityRepo::list_page(
        &conn,
        &allow_all(),
        &by_pattern(&pattern),
        PageRequest::first(10),
    )
    .await
    .expect("page");
    let ids: Vec<&str> = page.items.iter().map(|m| m.gts_id.as_str()).collect();
    assert_eq!(ids, vec![MATCH]);
    assert_eq!(
        page.next_after, None,
        "no continuation after the last match"
    );
}

// ---------------------------------------------------------------------------
// dependency closure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn closure_over_a_chain_returns_the_whole_chain_and_nothing_outside_it() {
    let db = test_db().await;
    let chain = [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.address.type.v1~"),
        gts_id!("acme.crm.country.type.v1~"),
    ];
    let outside = gts_id!("acme.crm.unrelated.type.v1~");
    let mut all: Vec<&str> = chain.to_vec();
    all.push(outside);
    seed(&db, &all).await;

    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let a = entity_id(&db, chain[0]).await;
    let b = entity_id(&db, chain[1]).await;
    let c = entity_id(&db, chain[2]).await;
    DependencyRepo::replace_outgoing(&conn, &scope, a, &[(DependencyKind::SchemaRef, b)])
        .await
        .expect("customer -> address");
    DependencyRepo::replace_outgoing(&conn, &scope, b, &[(DependencyKind::SchemaRef, c)])
        .await
        .expect("address -> country");

    let closure = DependencyRepo::closure(&conn, &scope, &[chain[0].to_owned()])
        .await
        .expect("closure");
    let got: Vec<&str> = closure.entities.iter().map(|m| m.gts_id.as_str()).collect();
    let mut expected: Vec<&str> = chain.to_vec();
    expected.sort_unstable();
    assert_eq!(
        got, expected,
        "the root plus everything it transitively consumes, gts_id-sorted"
    );
    assert!(!got.contains(&outside));
    assert!(closure.missing_roots.is_empty());
}

#[tokio::test]
async fn closure_terminates_on_a_row_that_contradicts_acyclicity() {
    let db = test_db().await;
    let pair = [
        gts_id!("acme.crm.a.type.v1~"),
        gts_id!("acme.crm.b.type.v1~"),
    ];
    seed(&db, &pair).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let a = entity_id(&db, pair[0]).await;
    let b = entity_id(&db, pair[1]).await;
    DependencyRepo::replace_outgoing(&conn, &scope, a, &[(DependencyKind::SchemaRef, b)])
        .await
        .expect("a -> b");
    DependencyRepo::replace_outgoing(&conn, &scope, b, &[(DependencyKind::SchemaRef, a)])
        .await
        .expect("b -> a");

    let closure = DependencyRepo::closure(&conn, &scope, &[pair[0].to_owned()])
        .await
        .expect("closure");
    assert_eq!(closure.entities.len(), 2);
}

/// A first admission's candidate has no entity row yet, so the closure read must
/// report it rather than fail: T5 builds its store from what exists plus the
/// candidate it is about to validate.
#[tokio::test]
async fn closure_reports_candidates_that_have_no_entity_row() {
    let db = test_db().await;
    seed(&db, &[CUSTOMER_V1]).await;
    let conn = db.conn().expect("conn");

    let closure = DependencyRepo::closure(
        &conn,
        &allow_all(),
        &[
            CUSTOMER_V1.to_owned(),
            gts_id!("acme.crm.brand.new.v1~").to_owned(),
        ],
    )
    .await
    .expect("closure");
    assert_eq!(
        closure
            .entities
            .iter()
            .map(|m| m.gts_id.as_str())
            .collect::<Vec<_>>(),
        vec![CUSTOMER_V1]
    );
    assert_eq!(
        closure.missing_roots,
        vec![gts_id!("acme.crm.brand.new.v1~")]
    );
}

#[tokio::test]
async fn closure_rejects_more_than_512_resolved_roots_before_the_first_hop() {
    let db = test_db().await;
    let roots: Vec<String> = (0..513)
        .map(|i| format!("gts.acme.crm.bound{i:03}.type.v1~"))
        .collect();
    let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    seed(&db, &root_refs).await;

    let conn = db.conn().expect("conn");
    let err = DependencyRepo::closure(&conn, &allow_all(), &roots)
        .await
        .expect_err("513 resolved roots must exceed the store-build bound");
    assert!(
        matches!(
            err,
            toolkit_db::secure::ScopeError::Invalid(message)
                if message.contains("512-entity store-build bound")
        ),
        "unexpected closure error: {err}"
    );
}

#[tokio::test]
async fn closure_refuses_a_fan_out_past_the_bound_and_admits_the_boundary() {
    let db = test_db().await;
    let hub = gts_id!("acme.crm.hub.type.v1~");
    let leaves: Vec<String> = (0..512)
        .map(|i| format!("gts.acme.crm.leaf{i:03}.type.v1~"))
        .collect();
    let mut all: Vec<&str> = vec![hub];
    all.extend(leaves.iter().map(String::as_str));
    seed(&db, &all).await;

    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let hub_id = entity_id(&db, hub).await;
    let leaf_ids: Vec<i64> = EntityRepo::find_by_gts_ids(&conn, &scope, &leaves)
        .await
        .expect("leaves")
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(leaf_ids.len(), 512);

    // 511 leaves plus the hub is exactly the bound, and must come back whole.
    let edges: Vec<(DependencyKind, i64)> = leaf_ids[..511]
        .iter()
        .map(|id| (DependencyKind::SchemaRef, *id))
        .collect();
    DependencyRepo::replace_outgoing(&conn, &scope, hub_id, &edges)
        .await
        .expect("511 edges");
    let closure = DependencyRepo::closure(&conn, &scope, &[hub.to_owned()])
        .await
        .expect("512 entities is the bound, not past it");
    assert_eq!(closure.entities.len(), 512);

    let edges: Vec<(DependencyKind, i64)> = leaf_ids
        .iter()
        .map(|id| (DependencyKind::SchemaRef, *id))
        .collect();
    DependencyRepo::replace_outgoing(&conn, &scope, hub_id, &edges)
        .await
        .expect("512 edges");
    let err = DependencyRepo::closure(&conn, &scope, &[hub.to_owned()])
        .await
        .expect_err("the hub plus 512 dependencies exceeds the store-build bound");
    assert!(
        matches!(err, ScopeError::Invalid(message) if message.contains("512-entity store-build bound")),
        "unexpected closure error: {err}"
    );
}

#[tokio::test]
async fn closure_refuses_rather_than_truncating_a_chain_deeper_than_the_bound() {
    let db = test_db().await;
    let chain: Vec<String> = (0..600)
        .map(|i| format!("gts.acme.crm.deep{i:03}.type.v1~"))
        .collect();
    let refs: Vec<&str> = chain.iter().map(String::as_str).collect();
    seed(&db, &refs).await;

    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let mut rows = EntityRepo::find_by_gts_ids(&conn, &scope, &chain)
        .await
        .expect("chain");
    // Zero-padded names, so lexicographic order is the chain's order.
    rows.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
    for pair in rows.windows(2) {
        DependencyRepo::replace_outgoing(
            &conn,
            &scope,
            pair[0].id,
            &[(DependencyKind::SchemaRef, pair[1].id)],
        )
        .await
        .expect("chain edge");
    }

    let err = DependencyRepo::closure(&conn, &scope, &[chain[0].clone()])
        .await
        .expect_err("a 600-deep chain is refused, never shortened to fit");
    assert!(
        matches!(err, ScopeError::Invalid(message) if message.contains("512-entity store-build bound")),
        "unexpected closure error: {err}"
    );
}

/// Admission replaces only the admitted entity's outgoing rows, so a second call
/// must not accumulate edges.
#[tokio::test]
async fn replace_outgoing_replaces_rather_than_accumulates() {
    let db = test_db().await;
    let ids = [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.address.type.v1~"),
        gts_id!("acme.crm.country.type.v1~"),
    ];
    seed(&db, &ids).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let a = entity_id(&db, ids[0]).await;
    let b = entity_id(&db, ids[1]).await;
    let c = entity_id(&db, ids[2]).await;

    DependencyRepo::replace_outgoing(&conn, &scope, a, &[(DependencyKind::SchemaRef, b)])
        .await
        .expect("first");
    DependencyRepo::replace_outgoing(&conn, &scope, a, &[(DependencyKind::SchemaRef, c)])
        .await
        .expect("second");

    let closure = DependencyRepo::closure(&conn, &scope, &[ids[0].to_owned()])
        .await
        .expect("closure");
    let got: Vec<&str> = closure.entities.iter().map(|m| m.gts_id.as_str()).collect();
    assert_eq!(
        got,
        vec![
            gts_id!("acme.crm.country.type.v1~"),
            gts_id!("acme.crm.customer.type.v1~")
        ],
        "the address edge must be gone, not merged with the country edge"
    );
}

/// `(from_entity_id, kind, to_entity_id)` is the primary key, so the edge list is
/// a set. A schema that `$ref`s the same base twice is an ordinary document, and
/// it must not become a primary-key violation partway through admission.
#[tokio::test]
async fn replace_outgoing_treats_a_repeated_edge_as_one() {
    let db = test_db().await;
    let ids = [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.address.type.v1~"),
    ];
    seed(&db, &ids).await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let from = entity_id(&db, ids[0]).await;
    let to = entity_id(&db, ids[1]).await;

    DependencyRepo::replace_outgoing(
        &conn,
        &scope,
        from,
        &[
            (DependencyKind::SchemaRef, to),
            (DependencyKind::SchemaRef, to),
        ],
    )
    .await
    .expect("a repeated edge is one edge, not a constraint violation");

    let rows = dependency::Entity::find()
        .filter(dependency::Column::FromEntityId.eq(from))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("edges");
    assert_eq!(
        rows.iter().map(|r| r.to_entity_id).collect::<Vec<_>>(),
        vec![to],
        "exactly one row, not two"
    );
}

// ---------------------------------------------------------------------------
// The same method inside a transaction
// ---------------------------------------------------------------------------

/// `runner: &impl DBRunner` exists so one method body serves both a pooled
/// connection and a transaction. A suite that only ever passes `DbConn` would not
/// notice a signature that had quietly narrowed to one of them.
#[tokio::test]
async fn the_same_repository_methods_run_inside_a_transaction() {
    let db = test_db().await;
    let committed = db
        .transaction(|tx| {
            Box::pin(async move {
                let scope = allow_all();
                let (family, created) = VersionFamilyRepo::create_or_get(
                    tx,
                    &scope,
                    FAMILY_KEY,
                    OwnershipScope::Global,
                    None,
                    NOW,
                )
                .await
                .expect("family inside tx");
                assert!(created);
                let entity = EntityRepo::insert(tx, &scope, new_entity(CUSTOMER_V1, family.id))
                    .await
                    .expect("entity inside tx")
                    .expect("the identifier is free");
                Ok::<_, DbError>(entity.id)
            })
        })
        .await
        .expect("transaction");

    let conn = db.conn().expect("conn");
    let row = EntityRepo::find_by_gts_id(&conn, &allow_all(), CUSTOMER_V1)
        .await
        .expect("read")
        .expect("committed row");
    assert_eq!(row.id, committed);
}

#[tokio::test]
async fn a_system_failure_fails_every_undecided_item_of_one_operation_and_no_other() {
    const SYSTEM_FAILURE: &str = r#"{"reason":"system_failure"}"#;

    let db = test_db().await;
    let conn = db.conn().expect("conn");
    let scope = allow_all();

    let operation = OperationRepo::insert(
        &conn,
        &scope,
        NewOperation {
            id: Uuid::new_v4(),
            kind: OperationKind::Registration,
            dry_run: false,
            plane: Plane::Platform,
            tenant_id: None,
            principal_id: Uuid::nil(),
            idempotency_key: "system-failure-batch".to_owned(),
            idempotency_scope_hash: ScopeHash::from_stored(vec![0x01; 32]).expect("32 bytes"),
            request_fingerprint: RequestFingerprint::from_stored(vec![0x02; 32]).expect("32 bytes"),
            now: NOW,
        },
    )
    .await
    .expect("insert operation");

    let items: Vec<NewOperationItem> = [CUSTOMER_V1, CUSTOMER_V1_DERIVED_A, CUSTOMER_V1_DERIVED_B]
        .iter()
        .enumerate()
        .map(|(index, gts_id)| NewOperationItem {
            item_no: i32::try_from(index).expect("three items"),
            gts_id: (*gts_id).to_owned(),
            precondition: Precondition::MustNotExist,
            compat_forced: false,
            request_payload: "{}".to_owned(),
        })
        .collect();
    OperationRepo::insert_items(&conn, &scope, &operation, &items)
        .await
        .expect("insert items");

    let seeded = OperationRepo::find_items(&conn, &scope, operation.id)
        .await
        .expect("read items");
    let decided = seeded
        .iter()
        .find(|item| item.gts_id == CUSTOMER_V1)
        .expect("the first candidate");
    assert!(
        OperationRepo::mark_item_succeeded(
            &conn,
            &scope,
            decided.id,
            ItemSuccess::Registered {
                revision_no: 1,
                resource_version: 1,
            },
            NOW,
        )
        .await
        .expect("terminalize one item")
    );

    let failed = OperationRepo::fail_nonterminal_items(
        &conn,
        &scope,
        operation.id,
        SYSTEM_FAILURE.to_owned(),
        NOW,
    )
    .await
    .expect("fail the undecided items");
    assert_eq!(
        failed, 2,
        "one statement must cover every undecided item, and only those",
    );

    let after = OperationRepo::find_items(&conn, &scope, operation.id)
        .await
        .expect("reread items");
    for item in &after {
        if item.gts_id == CUSTOMER_V1 {
            assert_eq!(
                item.status,
                OperationItemStatus::Succeeded,
                "an item an earlier pass decided keeps its outcome",
            );
            assert_eq!(item.result_revision_no, Some(1));
        } else {
            assert_eq!(item.status, OperationItemStatus::Failed, "{}", item.gts_id);
            assert_eq!(
                item.error_payload.as_deref(),
                Some(SYSTEM_FAILURE),
                "{}: every failed item carries the one system-failure reason",
                item.gts_id,
            );
        }
    }

    let again = OperationRepo::fail_nonterminal_items(
        &conn,
        &scope,
        operation.id,
        SYSTEM_FAILURE.to_owned(),
        NOW,
    )
    .await
    .expect("second failure write");
    assert_eq!(again, 0, "the guard makes a repeated failure write a no-op");
}

fn by_pattern(pattern: &GtsIdPattern) -> ListFilter {
    ListFilter {
        pattern: Some(pattern.clone()),
        ..ListFilter::default()
    }
}
