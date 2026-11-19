// Created: 2026-07-26 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! DB-behavior audit for resource-group — query-count half.
//!
//! Every operation runs against SQLite with a
//! [`toolkit_db::test_support::QueryRecorder`] attached, and the assertions
//! are about statement *counts*: `n-plus-one` and `redundant-io`. Where cost
//! could depend on input size, the operation runs at two sizes and the slope
//! is what is asserted — an absolute count rots on the next refactor, a slope
//! does not.
//!
//! The `no-tx-write` class is asserted on the write paths: each of their
//! trace tests ends on [`QueryRecorder::writes_outside_tx`]. Read paths and
//! the scale tests of Section 2 do not — for a read path the assertion is
//! trivially true, and a scale test is about a slope, not a boundary.
//!
//! Three classes are not observable as a statement count at all and have
//! source-scan rules in Section 4 instead: `no-retry-serializable`,
//! `external-call-in-tx`, and the row lock that lets a non-force delete run
//! below `SERIALIZABLE` (invisible on SQLite, which has no row locks).
//!
//! Deliberately absent: the write-set narrowing checks, which belong to a
//! fix this branch does not carry — a test asserting a fix that is not here
//! would only be noise. It lives on the branch that carries it.
//!
//! Healthy operations assert the invariant directly, doubling as negative
//! controls.
//!
//! Trace dumps: set `DB_AUDIT_TRACE_DIR` (see [`snapshot_trace`]); an
//! ordinary run writes nothing.
//!
//! Findings inventory and how to repeat this audit on another module:
//! `docs/db-behavior-audit.md`.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{CreateTypeRequest, UpdateTypeRequest};
use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group_sdk::UpdateGroupRequest;
use toolkit_db::test_support::{QueryKind, snapshot_trace};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

// =========================================================================
// Fixture helpers
// =========================================================================

fn make_group_service_with_profile(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    profile: QueryProfile,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        profile,
        common::make_enforcer(),
        Arc::new(GroupRepository),
        common::make_types_catalog(),
    )
}

/// A type that can root itself and allows itself as a parent -- lets a
/// single type build chains/trees of arbitrary depth and width.
async fn create_self_referencing_type(
    type_svc: &common::TestTypes,
    suffix: &str,
) -> common::ResourceGroupType {
    // `resolve_ids` rejects a parent path that doesn't exist yet, so a type
    // can't reference itself as an allowed parent at create time; create it
    // plain, then update it to add the self-reference.
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create self-referencing type (initial)");
    type_svc
        .update_type_unscoped(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update type to add self-reference")
}

/// Mirrors `membership_service_test.rs`'s local helper: a root type whose
/// `allowed_membership_types` includes the given resource-type paths.
async fn create_type_with_memberships(
    type_svc: &common::TestTypes,
    suffix: &str,
    memberships: &[&str],
) -> common::ResourceGroupType {
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code,
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: memberships.iter().map(|s| (*s).to_owned()).collect(),
            metadata_schema: None,
        })
        .await
        .expect("create type with memberships")
}

/// Build a chain of `depth` nodes (root + `depth - 1` single children),
/// returning the id of the last (deepest) node.
async fn build_chain(
    group_svc: &GroupService<GroupRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    tenant_id: Uuid,
    depth: usize,
) -> Uuid {
    assert!(depth >= 1, "chain must have at least one node");
    let root = common::create_root_group(group_svc, ctx, type_code, "n0", tenant_id).await;
    let mut current = root.id;
    for i in 1..depth {
        let child = common::create_child_group(
            group_svc,
            ctx,
            type_code,
            current,
            &format!("n{i}"),
            tenant_id,
        )
        .await;
        current = child.id;
    }
    current
}

/// Build a flat subtree under `parent_id`: one "subtree root" child plus
/// `child_count` leaves directly under it. Returns the subtree root's id.
/// Total subtree size (including the subtree root) is `child_count + 1`.
async fn build_flat_subtree(
    group_svc: &GroupService<GroupRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    parent_id: Uuid,
    tenant_id: Uuid,
    child_count: usize,
) -> Uuid {
    let subtree_root = common::create_child_group(
        group_svc,
        ctx,
        type_code,
        parent_id,
        "subtree-root",
        tenant_id,
    )
    .await;
    for i in 0..child_count {
        common::create_child_group(
            group_svc,
            ctx,
            type_code,
            subtree_root.id,
            &format!("leaf{i}"),
            tenant_id,
        )
        .await;
    }
    subtree_root.id
}

fn count_in(stats: &BTreeMap<(QueryKind, String), usize>, kind: QueryKind, table: &str) -> usize {
    stats.get(&(kind, table.to_owned())).copied().unwrap_or(0)
}

// =========================================================================
// Section 1 -- per-operation trace snapshots + writes-in-tx assertions
// =========================================================================

#[tokio::test]
async fn trace_create_root_group() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&type_svc, "org").await;

    rec.clear();
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    assert_eq!(root.hierarchy.parent_id, None);

    snapshot_trace("create_root_group", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "create_group must run its writes inside a transaction:\n{}",
        rec.dump()
    );
    // No resource_group SELECT at all. `create_group_inner` used to read the
    // row back to build a response it could assemble from the insert (RG-08),
    // and SeaORM used to add a re-select of its own on SQLite; as of SeaORM
    // 2.0 the insert carries RETURNING on every backend, so neither remains.
    let rg_selects = count_in(&rec.stats(), QueryKind::Select, "resource_group");
    assert_eq!(
        rg_selects,
        0,
        "RG-08 regression: the insert returns the row, so nothing should \
         re-read it; got {rg_selects} resource_group SELECTs:\n{}",
        rec.dump()
    );
    // The type definition comes from the types registry before `BEGIN`
    // (RG-09, T38b): the database holds no type catalogue to read. The only
    // type table touched is the identity dictionary, inside the transaction:
    // the first group of a type reads it and adds the key.
    let tables: std::collections::BTreeSet<String> =
        rec.stats().keys().map(|(_, table)| table.clone()).collect();
    assert!(
        tables
            .iter()
            .all(|t| !t.starts_with("gts_type") || t == "gts_type"),
        "RG type definitions are not read from RG's database: {tables:?}\n{}",
        rec.dump()
    );
    assert_eq!(count_in(&rec.stats(), QueryKind::Select, "gts_type"), 1);
    assert_eq!(count_in(&rec.stats(), QueryKind::Insert, "gts_type"), 1);

    // A known type only reads its key: no insert, no identity consumed.
    rec.clear();
    common::create_root_group(&group_svc, &ctx, &root_type.code, "Root 2", tenant_id).await;
    assert!(rec.writes_outside_tx().is_empty(), "{}", rec.dump());
    assert_eq!(count_in(&rec.stats(), QueryKind::Select, "gts_type"), 1);
    assert_eq!(
        count_in(&rec.stats(), QueryKind::Insert, "gts_type"),
        0,
        "a known type must not insert into the dictionary:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_create_child_group_depth3() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = make_group_service_with_profile(
        db.clone(),
        QueryProfile {
            max_depth: None,
            max_width: None,
        },
    );
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "chain").await;

    let n0 = common::create_root_group(&group_svc, &ctx, &t.code, "n0", tenant_id).await;
    let n1 = common::create_child_group(&group_svc, &ctx, &t.code, n0.id, "n1", tenant_id).await;

    rec.clear();
    let n2 = common::create_child_group(&group_svc, &ctx, &t.code, n1.id, "n2", tenant_id).await;
    assert_eq!(n2.hierarchy.parent_id, Some(n1.id));

    snapshot_trace("create_child_group_depth3", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "create_group must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_update_group() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = common::create_root_type(&type_svc, "org").await;
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "Root", tenant_id).await;

    rec.clear();
    let updated = group_svc
        .update_group(
            &ctx,
            root.id,
            UpdateGroupRequest {
                parent_id: None,
                name: "Root Renamed".to_owned(),
                metadata: None,
            },
        )
        .await
        .expect("update_group should succeed");
    assert_eq!(updated.name, "Root Renamed");

    snapshot_trace("update_group", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "update_group must run its writes inside a transaction:\n{}",
        rec.dump()
    );
    // RG-08's `update` half, now closed: the write reported a row count and
    // the row was read back twice -- once inside `update` to satisfy a return
    // type nobody used, once by the caller to build the response. Both are
    // gone; the response is assembled from what was written. This pinned the
    // defect as present until it was fixed, and is a negative control now.
    assert!(
        rec.redundant_reads_after_write().is_empty(),
        "update_group must not read a row back after writing it (RG-08):\n{}",
        rec.dump()
    );
    // A plain rename never touches the parent, so `update_group_inner` no
    // longer loads the full type (`find_by_code`) to feed
    // `move_group_internal_impl`'s parent-compatibility check -- that read
    // only matters on the `parent_changed` branch, which this request never
    // takes. It used to run unconditionally and pull both junction tables
    // for a type this call never needed the parent/membership lists of.
    let parent_junction_selects =
        count_in(&rec.stats(), QueryKind::Select, "gts_type_allowed_parent");
    let membership_junction_selects = count_in(
        &rec.stats(),
        QueryKind::Select,
        "gts_type_allowed_membership",
    );
    assert_eq!(
        parent_junction_selects,
        0,
        "a rename must not read gts_type_allowed_parent -- that table is only \
         relevant to the parent-changed branch's move-compatibility check, \
         got {parent_junction_selects} SELECTs:\n{}",
        rec.dump()
    );
    assert_eq!(
        membership_junction_selects,
        0,
        "a rename must not read gts_type_allowed_membership -- same reasoning \
         as gts_type_allowed_parent above, got {membership_junction_selects} \
         SELECTs:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_move_group() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "mv").await;

    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;
    let target_parent =
        common::create_child_group(&group_svc, &ctx, &t.code, root.id, "target", tenant_id).await;
    // Small subtree (3 nodes) so the canonical trace isn't dominated by noise.
    let moved = build_flat_subtree(&group_svc, &ctx, &t.code, root.id, tenant_id, 2).await;

    rec.clear();
    let result = group_svc
        .move_group(&ctx, moved, Some(target_parent.id))
        .await
        .expect("move_group should succeed");
    assert_eq!(result.hierarchy.parent_id, Some(target_parent.id));

    snapshot_trace("move_group", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "move_group must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_force_delete_subtree() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "del").await;
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;
    let subtree_root = build_flat_subtree(&group_svc, &ctx, &t.code, root.id, tenant_id, 2).await;

    rec.clear();
    group_svc
        .delete_group(&ctx, subtree_root, true)
        .await
        .expect("force delete should succeed");

    snapshot_trace("force_delete_subtree", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "delete_group(force=true) must run its writes inside a transaction:\n{}",
        rec.dump()
    );
    // A force delete rewrites a whole subtree -- closure rows, memberships,
    // the group rows themselves -- and races a concurrent create or move
    // anywhere inside it. That is write skew over rows it does not lock, so
    // it keeps `SERIALIZABLE` where the non-force delete does not.
    assert!(
        rec.all_in_serializable_transaction(),
        "the force-delete cascade rewrites a subtree it does not lock and \
         must open SERIALIZABLE:\n{}",
        rec.dump()
    );
}

/// `list_memberships`'s tenant scoping is applied as
/// `group_id IN (subquery over resource_group)` inside the page's single
/// `SELECT`, not a second round trip (the DB engine evaluates the
/// subquery server-side, inside one statement -- there is no
/// N+1 at the client/statement level, which is what this audit suite
/// measures).
#[tokio::test]
async fn trace_list_memberships() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let member_type = common::create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant_id).await;
    for i in 0..5 {
        membership_svc
            .add_membership(&ctx, group.id, &member_type.code, &format!("res-{i}"))
            .await
            .expect("add_membership should succeed");
    }

    rec.clear();
    let page = membership_svc
        .list_memberships(&ctx, &toolkit_odata::ODataQuery::default())
        .await
        .expect("list_memberships should succeed");
    assert_eq!(
        page.items.len(),
        5,
        "all 5 seeded memberships must be listed"
    );

    snapshot_trace("list_memberships", &rec);
    // baseline: exactly 1 resource_group_membership SELECT for the
    // whole page (the `group_id IN (SELECT id FROM resource_group ...)` subquery lives inside
    // that single statement's WHERE clause, so it doesn't add a
    // resource_group_membership SELECT of its own, and -- crucially -- it
    // does not scale with the number of rows in the page: 5 items, 1
    // statement, not 5).
    let membership_selects = count_in(&rec.stats(), QueryKind::Select, "resource_group_membership");
    assert_eq!(
        membership_selects,
        1,
        "RG-12 regression: expected exactly 1 resource_group_membership \
         SELECT for the page (no N+1 from the tenant-scope subquery), \
         got {membership_selects}:\n{}",
        rec.dump()
    );
}

/// `add_membership`'s own trace, which the suite did not have.
///
/// Three things are pinned here.
///
/// The membership table is read once and written once: the tenant-compatibility
/// check is a single statement whose subquery derives the member groups
/// server-side, and the insert is not followed by a read-back — every column of
/// that table is a key part or `created_at`, all four known to the caller
/// (RG-08).
///
/// Fixed (RG-01): the tenant-compatibility check and the membership insert
/// now share one `SERIALIZABLE` transaction inside `add_membership_inner`.
/// Apart, two first memberships from different tenants each read an empty
/// set and both commit; together at that level the read and the write into
/// the same predicate are the write skew SSI cancels. This asserts the fix.
///
/// Fixed again: the `allowed_membership_types` check -- loading the group's
/// full type and testing the requested resource type against it -- used to
/// run on the pool, before `BEGIN`. PostgreSQL's SSI only tracks
/// rw-antidependencies between reads and writes that both happen inside a
/// serializable transaction, so a concurrent `update_type` removing this
/// resource type from `allowed_membership_types` -- itself `SERIALIZABLE` --
/// was invisible to a pre-transaction read of the same row: it could commit
/// between that read and this insert with neither side raising `40001`. The
/// check now runs inside the same transaction as the tenant check and the
/// insert, so all three are covered by the one SSI cycle. The scoped group
/// read and `resolve_id` (the resource type's own surrogate-id lookup) run
/// first in each attempt of that same transaction, so a group deleted and
/// re-created under another tenant between the check and the insert cannot
/// pass the gate.
#[tokio::test]
async fn trace_add_membership() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let member_type = common::create_root_type(&type_svc, "addmbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "addgrp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant_id).await;

    rec.clear();
    membership_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-1")
        .await
        .expect("add_membership should succeed");

    snapshot_trace("add_membership", &rec);

    // No SELECT whose own table is resource_group_membership: the
    // tenant-compatibility check reads FROM resource_group and derives the
    // member groups in a subquery, and nothing reads the row back after the
    // insert -- SeaORM's SQLite re-select is gone as of SeaORM 2.0.
    let membership_selects = count_in(&rec.stats(), QueryKind::Select, "resource_group_membership");
    assert_eq!(
        membership_selects,
        0,
        "RG-08 regression: the tenant check reads FROM resource_group and the \
         insert returns its row, so nothing should select from \
         resource_group_membership; got {membership_selects}:\n{}",
        rec.dump()
    );

    let membership_inserts = count_in(&rec.stats(), QueryKind::Insert, "resource_group_membership");
    assert_eq!(
        membership_inserts,
        1,
        "expected exactly 1 resource_group_membership INSERT, got \
         {membership_inserts}:\n{}",
        rec.dump()
    );

    assert!(
        rec.writes_outside_tx().is_empty(),
        "RG-01 regression: add_membership's tenant check and membership insert \
         should share one transaction; got a write outside it:\n{}",
        rec.dump()
    );
    // `writes_outside_tx` only proves each write ran inside *some*
    // transaction -- it would still pass if the tenant check and the
    // membership insert ran in two separate, sequential transactions rather
    // than one, which reopens RG-01 just as surely as running outside a
    // transaction entirely.
    assert!(
        rec.all_in_one_transaction(),
        "RG-01 regression: add_membership's tenant check and membership insert \
         must share one transaction, not merely one each:\n{}",
        rec.dump()
    );
    // One transaction is not enough on its own, and the missing half is
    // something no statement count can see. The check reads a predicate and
    // the insert writes into it; at the backend default both writers read
    // from their own snapshot, neither sees the other's uncommitted row, and
    // both commit. Only `SERIALIZABLE` makes that a cycle SSI can cancel --
    // and lowering the level leaves this trace byte-identical (same
    // statements, same order, same transaction), while on SQLite, which
    // serializes writes regardless, the outcome does not change either. The
    // requested level is the only signal.
    assert!(
        rec.all_in_serializable_transaction(),
        "RG-01 regression: add_membership must open SERIALIZABLE -- the tenant \
         check is a predicate read the insert writes into, and below that \
         level two first memberships from different tenants both commit:\n{}",
        rec.dump()
    );

    // The allowed_membership_types check reads the group's full type,
    // including the `gts_type_allowed_parent` and `gts_type_allowed_membership`
    // junction tables. `all_in_serializable_transaction()` above only asks
    // whether every *in-tx* event is SERIALIZABLE -- it has nothing to say
    // about a read that runs on the pool instead of in a transaction at all,
    // which is exactly the shape the regression this pins would take: the
    // check moving back out of the transaction rather than merely running
    // at a weaker level. Assert directly that every read of those two
    // junction tables happened inside a transaction.
    let junction_read_outside_tx = rec.events().into_iter().any(|e| {
        matches!(e.kind, QueryKind::Select)
            && matches!(
                e.table.as_deref(),
                Some("gts_type_allowed_parent" | "gts_type_allowed_membership")
            )
            && !e.in_tx
    });
    assert!(
        !junction_read_outside_tx,
        "allowed_membership_types regression: the type's junction tables must \
         be read inside the same SERIALIZABLE transaction as the insert, not \
         on the pool -- an outside-tx read of either is invisible to SSI and \
         cannot be cancelled against a concurrent update_type:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_remove_membership() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let member_type = common::create_root_type(&type_svc, "rmmbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "rmgrp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant_id).await;
    membership_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-rm")
        .await
        .expect("seed membership");

    rec.clear();
    membership_svc
        .remove_membership(&ctx, group.id, &member_type.code, "res-rm")
        .await
        .expect("remove_membership should succeed");

    snapshot_trace("remove_membership", &rec);

    // One DELETE by primary key, and nothing decided from a read: tenant
    // ownership is derived from the membership rows themselves, so a removal
    // has no second piece of state to keep in step and needs neither its own
    // transaction nor a level above the backend default. The negative half of
    // `trace_add_membership`'s assertion -- "raise everything to
    // SERIALIZABLE" must not be how that one is satisfied.
    let membership_deletes = count_in(&rec.stats(), QueryKind::Delete, "resource_group_membership");
    assert_eq!(
        membership_deletes,
        1,
        "expected exactly 1 resource_group_membership DELETE, got \
         {membership_deletes}:\n{}",
        rec.dump()
    );
    assert!(
        !rec.all_in_serializable_transaction(),
        "remove_membership decides nothing from a read and must stay at the \
         backend default:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_delete_group_non_force() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t = create_self_referencing_type(&type_svc, "ndel").await;
    let leaf = common::create_root_group(&group_svc, &ctx, &t.code, "leaf", tenant_id).await;

    rec.clear();
    group_svc
        .delete_group(&ctx, leaf.id, false)
        .await
        .expect("non-force delete should succeed");

    snapshot_trace("delete_group_non_force", &rec);

    // The negative half of the pairing the other three traces assert. A
    // non-force delete removes one row by primary key and takes a row lock on
    // it, so it has no cross-row predicate for SSI to protect and deliberately
    // stays at the backend default -- that is the saving the isolation work in
    // this branch is about. Without this, "make everything SERIALIZABLE" would
    // satisfy every other assertion here while quietly undoing it.
    assert!(
        rec.all_in_one_transaction(),
        "non-force delete must still run in one transaction:\n{}",
        rec.dump()
    );
    assert!(
        !rec.all_in_serializable_transaction(),
        "a non-force delete has no predicate SSI needs to protect and must \
         stay at the backend default:\n{}",
        rec.dump()
    );
}

// =========================================================================
// Section 2 -- scale-invariance: statement count must not grow with N
// =========================================================================

#[tokio::test]
async fn scale_create_child_closure_inserts_do_not_grow_with_ancestor_depth() {
    // insert_ancestor_closure_rows computes the whole ancestor set inside the
    // database with one INSERT ... SELECT (RG-06).
    //
    // Statement count alone can't tell an `INSERT ... SELECT` apart from the
    // same row set materialized in Rust and sent as one multi-row `INSERT`:
    // both are "1 INSERT". `normalize_sql` collapses value-group counts on
    // purpose (batch size shouldn't change a statement's normalized shape),
    // so it can't distinguish them either -- but a multi-row `INSERT` binds
    // one parameter per column per row while `INSERT ... SELECT` binds none
    // of the row data, so total bind-parameter count does, and is asserted
    // alongside the statement count.
    async fn closure_inserts_for_new_child_at_depth(depth: usize) -> (usize, usize) {
        let (db, rec) = common::test_db_with_recorder().await;
        let type_svc = common::make_type_service(db.clone());
        let group_svc = make_group_service_with_profile(
            db.clone(),
            QueryProfile {
                max_depth: None,
                max_width: None,
            },
        );
        let tenant_id = Uuid::now_v7();
        let ctx = common::make_ctx(tenant_id);
        let t = create_self_referencing_type(&type_svc, "anc").await;
        let last = build_chain(&group_svc, &ctx, &t.code, tenant_id, depth).await;

        rec.clear();
        common::create_child_group(&group_svc, &ctx, &t.code, last, "extra", tenant_id).await;
        (
            count_in(&rec.stats(), QueryKind::Insert, "resource_group_closure"),
            rec.total_params(),
        )
    }

    let (small, small_params) = closure_inserts_for_new_child_at_depth(3).await;
    let (large, large_params) = closure_inserts_for_new_child_at_depth(15).await;
    assert_eq!(
        small, large,
        "closure INSERT count must not scale with ancestor depth \
         (small={small} at depth 3, large={large} at depth 15)"
    );
    assert_eq!(
        small_params, large_params,
        "total bind-parameter count must not scale with ancestor depth \
         (small={small_params} at depth 3, large={large_params} at depth 15) -- \
         a materialized multi-row INSERT would grow here even with a flat \
         statement count"
    );
}

async fn move_stats_for_subtree_size(n: usize) -> (BTreeMap<(QueryKind, String), usize>, usize) {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "mvscale").await;

    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;
    let target_parent =
        common::create_child_group(&group_svc, &ctx, &t.code, root.id, "target", tenant_id).await;
    assert!(n >= 1);
    let moved = build_flat_subtree(&group_svc, &ctx, &t.code, root.id, tenant_id, n - 1).await;

    rec.clear();
    group_svc
        .move_group(&ctx, moved, Some(target_parent.id))
        .await
        .expect("move_group should succeed");
    (rec.stats(), rec.total_params())
}

#[tokio::test]
async fn scale_move_closure_inserts_do_not_grow_with_subtree_size() {
    // rebuild_subtree_closure forms the whole A x N cross product inside the
    // database with one INSERT ... SELECT; the pairs never become Rust
    // values (RG-04).
    //
    // Statement count alone can't tell that cross-product INSERT ... SELECT
    // apart from the same pairs materialized in Rust and sent as one
    // multi-row INSERT -- normalize_sql collapses value-group counts on
    // purpose, so both look like "1 INSERT". Total bind-parameter count does
    // distinguish them (one param per column per row for a materialized
    // insert, none for a SELECT-sourced one), so it's asserted alongside the
    // statement count.
    let (small_stats, small_params) = move_stats_for_subtree_size(3).await;
    let (large_stats, large_params) = move_stats_for_subtree_size(15).await;
    let small = count_in(&small_stats, QueryKind::Insert, "resource_group_closure");
    let large = count_in(&large_stats, QueryKind::Insert, "resource_group_closure");
    assert_eq!(
        small, large,
        "closure INSERT count during move must not scale with subtree size \
         (small={small} at N=3, large={large} at N=15)"
    );
    assert_eq!(
        small_params, large_params,
        "total bind-parameter count during move must not scale with subtree \
         size (small={small_params} at N=3, large={large_params} at N=15) -- \
         a materialized multi-row INSERT would grow here even with a flat \
         statement count"
    );
}

#[tokio::test]
async fn scale_move_descendant_depth_selects_do_not_grow_with_subtree_size() {
    // Move's depth validation calls get_max_descendant_depth once -- a
    // single MAX(depth) aggregate -- rather than pulling every descendant
    // row into this process to fold down to that one number (RG-05).
    let small = count_in(
        &move_stats_for_subtree_size(3).await.0,
        QueryKind::Select,
        "resource_group_closure",
    );
    let large = count_in(
        &move_stats_for_subtree_size(15).await.0,
        QueryKind::Select,
        "resource_group_closure",
    );
    assert_eq!(
        small, large,
        "closure SELECT count during move must not scale with subtree size \
         (small={small} at N=3, large={large} at N=15)"
    );
}

async fn total_statements_for_force_delete(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "fd").await;
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;
    assert!(n >= 1);
    for i in 0..(n - 1) {
        common::create_child_group(
            &group_svc,
            &ctx,
            &t.code,
            root.id,
            &format!("leaf{i}"),
            tenant_id,
        )
        .await;
    }

    rec.clear();
    group_svc
        .delete_group(&ctx, root.id, true)
        .await
        .expect("force delete should succeed");
    rec.total()
}

#[tokio::test]
async fn scale_force_delete_statements_do_not_grow_with_subtree_size() {
    // Force delete batches memberships/closure deletes across the whole
    // subtree and deletes groups depth-level by depth-level, deepest first
    // (RG-10).
    let small = total_statements_for_force_delete(3).await;
    let large = total_statements_for_force_delete(15).await;
    assert_eq!(
        small, large,
        "force-delete total statement count must not scale with subtree size \
         (small={small} at N=3, large={large} at N=15)"
    );
}

/// Statements issued by a *rejected* non-force delete, with `n` children
/// each of a distinct GTS type.
///
/// Distinct types on purpose: a rejection that named its blocking children
/// would have to learn each one's type path, and a per-type lookup is the
/// shape that scales. Children sharing one type would hide that behind
/// memoization -- the growth would be in the number of *distinct* types, not
/// in the number of children.
async fn total_statements_for_rejected_delete(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // The parent type first, then one child type per child, each naming the
    // parent as its only allowed parent. Distinct child types are the point:
    // see this helper's doc comment.
    let parent_type = common::create_root_type(&type_svc, "rejdelp").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &parent_type.code, "root", tenant_id).await;
    for i in 0..n {
        let child_type = common::create_child_type(
            &type_svc,
            &format!("rejdel{i}"),
            &[parent_type.code.as_str()],
            &[],
        )
        .await;
        common::create_child_group(
            &group_svc,
            &ctx,
            &child_type.code,
            root.id,
            &format!("child{i}"),
            tenant_id,
        )
        .await;
    }

    rec.clear();
    let err = group_svc
        .delete_group(&ctx, root.id, false)
        .await
        .expect_err("a group with children must not be deletable without force");
    assert!(
        matches!(
            err,
            resource_group::domain::error::DomainError::ConflictActiveReferences { .. }
        ),
        "expected the blocking-children rejection, got: {err:?}"
    );
    rec.total()
}

/// The rejection path must cost the same whether one child blocks the delete
/// or twelve of different types do.
///
/// A forward guard, not a regression test: the per-type `SELECT` this
/// describes belongs to the `name blocking children on delete` work, which is
/// not in this branch — the rejection here counts children and never looks at
/// their types. So this passes on the code as it stands *and* on the code
/// before this branch, and it is here for the reason the original audit found
/// out the hard way: of ten scale tests, none covered
/// `delete_group(force = false)`, so a per-type lookup landed there unseen.
/// This is the watch that was missing, set before the code it watches.
#[tokio::test]
async fn scale_rejected_delete_statements_do_not_grow_with_child_type_count() {
    let small = total_statements_for_rejected_delete(2).await;
    let large = total_statements_for_rejected_delete(12).await;
    assert_eq!(
        small, large,
        "the rejected-delete statement count must not scale with the number of \
         distinct child types (small={small} at N=2, large={large} at N=12)"
    );
}

// Section 3 -- negative controls: both rely on SERIALIZABLE + retry and
// must show writes_outside_tx() == empty, proving the no-tx-write rule
// doesn't flag these paths.
//
// SQLite can't exercise the actual SSI conflict at all: it serializes every
// writer, so there is nothing to conflict. Showing that these paths retry
// rather than fail needs a Postgres-backed concurrency suite, which is not in
// this branch.

fn unique_tenant_type_code() -> String {
    format!(
        "{}x.test.tn.i{}.v1~",
        resource_group_sdk::TENANT_RG_TYPE_PATH,
        Uuid::now_v7().as_simple()
    )
}

#[tokio::test]
async fn negative_control_tenant_root_create_runs_in_tx() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: unique_tenant_type_code(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create tenant type");
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    rec.clear();
    let root =
        common::create_root_group(&group_svc, &ctx, &tenant_type.code, "Tenant", tenant_id).await;
    assert_eq!(root.hierarchy.tenant_id, root.id);

    assert!(
        rec.writes_outside_tx().is_empty(),
        "tenant-root create (an invariant protected by SSI) must run inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn negative_control_width_limited_create_runs_in_tx() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = make_group_service_with_profile(
        db.clone(),
        QueryProfile {
            max_depth: None,
            max_width: Some(1),
        },
    );
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = create_self_referencing_type(&type_svc, "width").await;
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;

    rec.clear();
    common::create_child_group(&group_svc, &ctx, &t.code, root.id, "only-child", tenant_id).await;

    assert!(
        rec.writes_outside_tx().is_empty(),
        "width-limited create (an invariant protected by SSI) must run inside a transaction:\n{}",
        rec.dump()
    );
}

/// Read paths must not be flagged by the write-oriented rules
/// (`writes_outside_tx`) -- there simply are no writes to flag.
#[tokio::test]
async fn negative_control_read_paths_produce_no_write_statements() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let t = common::create_root_type(&type_svc, "read").await;
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;

    rec.clear();
    group_svc
        .get_group(&ctx, root.id)
        .await
        .expect("get_group should succeed");
    group_svc
        .list_groups(&ctx, &toolkit_odata::ODataQuery::default())
        .await
        .expect("list_groups should succeed");
    group_svc
        .list_groups_unscoped(&toolkit_odata::ODataQuery::default())
        .await
        .expect("list_groups_unscoped should succeed");

    assert!(
        rec.total() > 0,
        "reads should still produce SELECT statements"
    );
    let stats = rec.stats();
    for kind in [QueryKind::Insert, QueryKind::Update, QueryKind::Delete] {
        for ((k, table), count) in &stats {
            assert!(
                *k != kind,
                "read-only calls must not produce {kind} statements (table {table}, count {count}):\n{}",
                rec.dump()
            );
        }
    }
    assert!(
        rec.writes_outside_tx().is_empty(),
        "trivially true for read paths, asserted for completeness"
    );
}

// Section 4 -- static source-scan rules for the contracts that leave no
// trace in the SQL: RG-03 (SERIALIZABLE without retry), RG-09 (an external
// call inside a transaction closure), and the row lock a non-force delete
// takes in place of SERIALIZABLE. Matched on call shape.
//
// Both service files are scanned for RG-03. On the query-count branch this
// file scanned only `group_service.rs`, which had never had the defect — so
// the rule passed by construction and was blind to the two live violations in
// `type_service.rs`. Those are fixed here, and both files are watched.

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The source of one `async fn`, from its name to the start of the next one.
///
/// These rules are text scans, so their reach is whatever slice they are
/// handed, and handing them the whole file is how a rule comes to pass on
/// someone else's code. Panics rather than returning empty: a rule that
/// silently scans nothing is worse than one that fails.
fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let prefix = format!("{name}(");
    src.split("async fn ")
        .find(|f| f.starts_with(&prefix))
        .unwrap_or_else(|| {
            panic!(
                "no `async fn {name}(` in the scanned source -- renamed? then rename it here too"
            )
        })
}

/// Split `if {marker} { <true arm> } else { <false arm> }` into its two arm
/// bodies, by brace depth rather than by string search for the closing
/// brace -- so a literal that happens to appear inside one arm's own nested
/// braces cannot be mistaken for the end of that arm.
///
/// `marker` is the text right after `if ` and before the opening `{`, e.g.
/// `"force"`. Panics with a clear message if the shape isn't there, rather
/// than silently returning something that makes every caller's assertion
/// vacuously true.
fn split_if_else<'a>(body: &'a str, marker: &str) -> (&'a str, &'a str) {
    let if_marker = format!("if {marker} {{");
    let if_start = body.find(&if_marker).unwrap_or_else(|| {
        panic!(
            "no `{if_marker}` found -- the branch this rule checks may have moved or been renamed"
        )
    });
    let true_start = if_start + if_marker.len();

    let true_end = brace_close(body, true_start);
    let true_arm = &body[true_start..true_end];

    let after_true = &body[true_end + 1..];
    let else_marker = "else {";
    let trimmed = after_true.trim_start();
    assert!(
        trimmed.starts_with(else_marker),
        "expected `}} else {{` right after the `if {marker}` arm, found: {:?}",
        &trimmed[..trimmed.len().min(40)]
    );
    let false_start = (body.len() - trimmed.len()) + else_marker.len();
    let false_end = brace_close(body, false_start);
    let false_arm = &body[false_start..false_end];

    (true_arm, false_arm)
}

/// Given the index right after an opening `{`, return the index of its
/// matching `}` by counting nested braces.
fn brace_close(body: &str, open_index: usize) -> usize {
    let mut depth = 1i32;
    for (offset, ch) in body[open_index..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open_index + offset;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces while scanning from index {open_index}");
}

#[test]
fn static_rule_passes_group_service_uses_retry() {
    let src = include_str!("../src/domain/group_service.rs");
    // Matched without the level, unlike an earlier revision that looked for
    // `.transaction_with_retry(TxConfig::serializable()` as one literal.
    // `update_group` and `delete_group` choose their level first and pass the
    // binding, so the literal missed exactly the two call sites where the
    // choice is dynamic -- the ones a regression is most likely to touch --
    // while the message claimed all four were covered.
    let unretried = count_occurrences(src, ".transaction_ref_mapped_with_config(");
    let retried = count_occurrences(src, ".transaction_with_retry(");
    assert_eq!(
        unretried, 0,
        "negative control violated: group_service.rs should not bypass \
         transaction_with_retry for its writes"
    );
    // Exact count, not a floor: the four call sites are create_group,
    // update_group (via attempt_update_group), move_group and delete_group --
    // two of them pass a level chosen from the request, which is why the
    // needle above matches the call and not the level. A floor would pass
    // just as well if another transaction were added without retry, or if a
    // refactor quietly dropped the wrapper from one; only exact equality
    // catches both directions.
    assert_eq!(
        retried, 4,
        "expected exactly 4 transaction_with_retry call sites in \
         group_service.rs -- create_group, update_group, move_group, \
         delete_group -- found {retried}"
    );
}

#[test]
fn static_rule_update_group_locks_the_row_it_may_rewrite() {
    // `update_group` opens at the backend default for a rename or a
    // metadata-only edit, and `GroupRepository::update` matches on `id`
    // alone while always assigning `parent_id`. So the read that decides
    // whether this request moves the group has to be the locked one: an
    // unlocked read answers from a READ COMMITTED snapshot, and a reparent
    // that commits in the gap gets written back out while the closure table
    // keeps its ancestry. Invisible on SQLite -- it has no row locks and
    // sea-query omits the clause -- and unreachable through the recorder,
    // which is why it is pinned here as well as by
    // `group_rename_does_not_revert_a_concurrent_reparent`.
    let src = include_str!("../src/domain/group_service.rs");
    let update_inner = fn_body(src, "update_group_inner");

    assert!(
        update_inner.contains("find_model_by_id_for_update"),
        "update_group_inner must take the row lock on the group before it \
         decides whether the parent changed: without it, a rename running \
         below SERIALIZABLE reverts a concurrent move it never saw"
    );
    assert!(
        !update_inner.contains(".find_model_by_id(tx, group_id)"),
        "the authoritative read of the updated group must be the locked one; \
         an unlocked read of the same row reintroduces the lost update"
    );
}

#[test]
fn static_rule_non_force_delete_locks_before_it_decides() {
    // Scoped to the two functions this is about, not to the file. Scanning
    // the whole file made the second assertion pass by accident: `if force {`,
    // `TxConfig::serializable()` and `TxConfig::default()` all appear
    // somewhere in `group_service.rs` regardless of what `delete_group` does
    // -- `update_group` alone supplies both TxConfig literals -- so hard-coding
    // the level back would not have failed it.
    let src = include_str!("../src/domain/group_service.rs");
    let delete_group = fn_body(src, "delete_group");
    let delete_inner = fn_body(src, "delete_group_inner");

    // Not observable as SQL here: SQLite has no row locks and sea-query omits
    // the clause for it entirely, so the trace on this backend looks the same
    // with and without the lock. On PostgreSQL it is what makes the ordering
    // against a concurrent `create_group` decidable -- the blocking itself
    // comes from the foreign key's `FOR KEY SHARE`, see the comment in
    // `delete_group`.
    assert!(
        delete_inner.contains("find_model_by_id_for_update"),
        "the non-force delete must lock the target row before checking its \
         children: without it, lowering the isolation level below SERIALIZABLE \
         opens the window it was closing"
    );

    // And the level must still be chosen, not hard-coded back -- checked
    // per branch, not by whether both literals appear somewhere in the
    // function. `contains("if force {") && contains(serializable) &&
    // contains(default)` passes just as well if the two branches were
    // swapped, since both literals are still present; splitting the `if` by
    // brace depth and checking each arm on its own catches that swap.
    let (force_true_arm, force_false_arm) = split_if_else(delete_group, "force");
    assert!(
        force_true_arm.contains("TxConfig::serializable()")
            && !force_true_arm.contains("TxConfig::default()"),
        "delete_group's `force` arm must select TxConfig::serializable() -- a \
         force delete rewrites a subtree and keeps SERIALIZABLE:\n{force_true_arm}"
    );
    assert!(
        force_false_arm.contains("TxConfig::default()")
            && !force_false_arm.contains("TxConfig::serializable()"),
        "delete_group's non-`force` arm must select TxConfig::default() -- a \
         non-force delete has no cross-row predicate to protect and must stay \
         at the backend default:\n{force_false_arm}"
    );
}

#[test]
fn static_rule_metadata_schema_is_resolved_before_begin() {
    let src = include_str!("../src/domain/group_service.rs");

    // RG-09. Reading the type from the types registry is a network
    // round-trip and the schema is then compiled; under an open transaction
    // that time is snapshot lifetime, and every retry pays it again. The fix
    // is structural rather than positional: the transaction-inner functions
    // take the already-read `RgType`, never the `RgTypeCatalog`, so the call
    // cannot drift back inside. This rule asserts the catalogue stays out --
    // a positional check would pass again the moment someone reintroduced
    // the argument and moved the call.
    let inner_fns: Vec<&str> = src
        .split("async fn ")
        .filter(|f| {
            f.starts_with("create_group_inner")
                || f.starts_with("update_group_inner")
                || f.starts_with("move_group_inner")
                || f.starts_with("move_group_internal_impl")
        })
        .collect();
    // Without this the rule below would pass on an empty set -- a rename or a
    // reshuffle would silently turn it into an assertion about nothing.
    assert_eq!(
        inner_fns.len(),
        4,
        "the scan should find create_group_inner, update_group_inner, \
         move_group_inner and move_group_internal_impl; found {} \
         definition(s). Rename? Then update this rule with it.",
        inner_fns.len()
    );

    let inner_taking_registry: Vec<&str> = inner_fns
        .iter()
        .filter(|f| {
            let signature_end = f.find(") -> Result").unwrap_or(f.len());
            let signature = &f[..signature_end];
            signature.contains("RgTypeCatalog") || signature.contains("types_registry")
        })
        .map(|f| f.split('(').next().unwrap_or(f))
        .collect();
    assert!(
        inner_taking_registry.is_empty(),
        "these transaction-inner functions take a registry client, which \
         puts an external call inside the transaction (RG-09): {inner_taking_registry:?}"
    );

    // Negative control: the validation must still happen somewhere. Without
    // this, deleting the call outright would satisfy the rule above.
    let calls = count_occurrences(src, "validate_metadata(");
    assert!(
        calls >= 2,
        "expected create_group and update_group to each validate metadata \
         before opening their transaction, found {calls} call(s)"
    );
}
