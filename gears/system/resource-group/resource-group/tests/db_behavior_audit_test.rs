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
//! The `no-tx-write` class is asserted throughout: every operation test ends
//! on [`QueryRecorder::writes_outside_tx`], and `no-retry-serializable` has
//! its own source-scan rule in Section 4.
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

use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::seeding::{self, GroupSeedDef};
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::{CreateTypeRequest, UpdateGroupRequest, UpdateTypeRequest};
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
) -> GroupService<GroupRepository, TypeRepository> {
    GroupService::new(
        db,
        profile,
        common::make_enforcer(),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        common::make_types_registry(),
    )
}

/// A type that can root itself and allows itself as a parent -- lets a
/// single type build chains/trees of arbitrary depth and width.
async fn create_self_referencing_type(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
) -> resource_group_sdk::ResourceGroupType {
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
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create self-referencing type (initial)");
    type_svc
        .update_type(
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
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
    memberships: &[&str],
) -> resource_group_sdk::ResourceGroupType {
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
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
    group_svc: &GroupService<GroupRepository, TypeRepository>,
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
    group_svc: &GroupService<GroupRepository, TypeRepository>,
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
    // Exactly 1 resource_group SELECT, and it is not ours: SeaORM's
    // non-RETURNING-fallback re-select after the insert (no
    // `sqlite-use-returning-for-3_35`), which PostgreSQL does not issue at
    // all. `create_group_inner` used to add a second, reading the row back to
    // build the response it could assemble from the insert (RG-08).
    let rg_selects = count_in(&rec.stats(), QueryKind::Select, "resource_group");
    assert_eq!(
        rg_selects,
        1,
        "RG-08 regression: expected only SeaORM's non-RETURNING fallback, \
         got {rg_selects} resource_group SELECTs:\n{}",
        rec.dump()
    );
    // Exactly 1 gts_type SELECT: `find_by_code_with_id`'s combined id+type
    // lookup (RG-11). The second one belonged to the response read-back,
    // which resolved the very code this request supplied -- it went with the
    // read-back itself (RG-08).
    let type_selects = count_in(&rec.stats(), QueryKind::Select, "gts_type");
    assert_eq!(
        type_selects,
        1,
        "RG-11 regression: expected exactly 1 gts_type SELECT (the combined \
         find_by_code_with_id lookup), got {type_selects}:\n{}",
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
        .move_group(moved, Some(target_parent.id))
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
}

#[tokio::test]
async fn trace_create_type() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());

    rec.clear();
    let t = common::create_root_type(&type_svc, "newtype").await;
    assert!(t.can_be_root);

    snapshot_trace("create_type", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "create_type's writes run inside a transaction (RG-03's missing retry \
         wrapper -- not trace-observable -- is fixed too, see the static rule \
         tests at the bottom of this file):\n{}",
        rec.dump()
    );
    // Exactly 2 gts_type SELECTs: the "does this code already exist"
    // pre-check plus SeaORM's non-RETURNING-fallback re-select
    // (absent on PostgreSQL).
    let type_selects = count_in(&rec.stats(), QueryKind::Select, "gts_type");
    assert_eq!(
        type_selects,
        2,
        "RG-08 regression: expected exactly 2 gts_type SELECTs (the exists-check + \
         SeaORM's non-RETURNING fallback), got {type_selects}:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_update_type() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let t = common::create_root_type(&type_svc, "upd").await;

    rec.clear();
    let updated = type_svc
        .update_type(
            &t.code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: Some(serde_json::json!({"type": "object"})),
            },
        )
        .await
        .expect("update_type should succeed");
    assert_eq!(
        updated.metadata_schema,
        Some(serde_json::json!({"type": "object"}))
    );

    snapshot_trace("update_type", &rec);
    assert!(
        rec.writes_outside_tx().is_empty(),
        "update_type's writes run inside a transaction (RG-03's retry wrapper, \
         not trace-observable, is fixed too):\n{}",
        rec.dump()
    );
    // Exactly 2 gts_type SELECTs: find_by_code_with_id's combined lookup,
    // plus the necessary post-update_many re-read (`SecureUpdateMany`
    // reports only rows-affected, never the model).
    let type_selects = count_in(&rec.stats(), QueryKind::Select, "gts_type");
    assert_eq!(
        type_selects,
        2,
        "RG-11 regression: expected exactly 2 gts_type SELECTs (the combined \
         find_by_code_with_id lookup + the necessary post-update re-read), got \
         {type_selects}:\n{}",
        rec.dump()
    );
}

/// `list_memberships`'s tenant scoping runs as a correlated EXISTS
/// subquery embedded in the page's single `SELECT`, not a second round trip
/// and not one subquery evaluation per row (the DB engine evaluates the
/// EXISTS per candidate row server-side, inside one statement -- there is no
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
    // whole page (the EXISTS subquery against resource_group lives inside
    // that single statement's WHERE clause, so it doesn't add a
    // resource_group_membership SELECT of its own, and -- crucially -- it
    // does not scale with the number of rows in the page: 5 items, 1
    // statement, not 5).
    let membership_selects = count_in(&rec.stats(), QueryKind::Select, "resource_group_membership");
    assert_eq!(
        membership_selects,
        1,
        " regression: expected exactly 1 resource_group_membership \
         SELECT for the page (no N+1 from the per-row tenant-scope subquery), \
         got {membership_selects}:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_seeding() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();

    let type_code = format!(
        "{}x.test.seed.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        Uuid::now_v7().as_simple()
    );

    rec.clear();
    let type_seeds = vec![CreateTypeRequest {
        code: type_code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    }];
    let type_result = seeding::seed_types(&type_svc, &type_seeds)
        .await
        .expect("seed_types should succeed");
    assert_eq!(type_result.created, 1);

    let root_id = Uuid::now_v7();
    let group_seeds = vec![GroupSeedDef {
        id: root_id,
        code: type_code,
        name: "Seeded Root".to_owned(),
        parent_id: None,
        metadata: None,
        tenant_id,
    }];
    let group_result = seeding::seed_groups(&group_svc, &group_seeds)
        .await
        .expect("seed_groups should succeed");
    assert_eq!(group_result.created, 1);

    // Membership seeding shares add_membership_inner's transaction behavior;
    // trace_add_membership asserts it directly, not repeated here.
    snapshot_trace("seeding", &rec);
}

// =========================================================================
// Section 2 -- scale-invariance: statement count must not grow with N
// =========================================================================

#[tokio::test]
async fn scale_create_child_closure_inserts_do_not_grow_with_ancestor_depth() {
    // insert_ancestor_closure_rows batches the whole ancestor set into one
    // secure_insert_many call (RG-06).
    async fn closure_inserts_for_new_child_at_depth(depth: usize) -> usize {
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
        count_in(&rec.stats(), QueryKind::Insert, "resource_group_closure")
    }

    let small = closure_inserts_for_new_child_at_depth(3).await;
    let large = closure_inserts_for_new_child_at_depth(15).await;
    assert_eq!(
        small, large,
        "closure INSERT count must not scale with ancestor depth \
         (small={small} at depth 3, large={large} at depth 15)"
    );
}

async fn move_stats_for_subtree_size(n: usize) -> BTreeMap<(QueryKind, String), usize> {
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
        .move_group(moved, Some(target_parent.id))
        .await
        .expect("move_group should succeed");
    rec.stats()
}

#[tokio::test]
async fn scale_move_closure_inserts_do_not_grow_with_subtree_size() {
    // rebuild_subtree_closure sends the whole A x N batch as a single
    // secure_insert_many call (RG-04).
    let small = count_in(
        &move_stats_for_subtree_size(3).await,
        QueryKind::Insert,
        "resource_group_closure",
    );
    let large = count_in(
        &move_stats_for_subtree_size(15).await,
        QueryKind::Insert,
        "resource_group_closure",
    );
    assert_eq!(
        small, large,
        "closure INSERT count during move must not scale with subtree size \
         (small={small} at N=3, large={large} at N=15)"
    );
}

#[tokio::test]
async fn scale_move_descendant_depth_selects_do_not_grow_with_subtree_size() {
    // Move's depth validation calls get_descendant_ids_with_depth once and
    // takes the max in memory (RG-05).
    let small = count_in(
        &move_stats_for_subtree_size(3).await,
        QueryKind::Select,
        "resource_group_closure",
    );
    let large = count_in(
        &move_stats_for_subtree_size(15).await,
        QueryKind::Select,
        "resource_group_closure",
    );
    assert_eq!(
        small, large,
        "closure SELECT count during move must not scale with subtree size \
         (small={small} at N=3, large={large} at N=15)"
    );
}

async fn junction_inserts_for_parent_count(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let mut parent_codes = Vec::with_capacity(n);
    for i in 0..n {
        let t = common::create_root_type(&type_svc, &format!("par{i}")).await;
        parent_codes.push(t.code);
    }

    rec.clear();
    type_svc
        .create_type(CreateTypeRequest {
            code: format!(
                "{}x.test.child.i{}.v1~",
                gts_id!("cf.core.rg.type.v1~"),
                Uuid::now_v7().as_simple()
            ),
            can_be_root: false,
            allowed_parent_types: parent_codes,
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create_type with N allowed parents should succeed");

    count_in(&rec.stats(), QueryKind::Insert, "gts_type_allowed_parent")
}

#[tokio::test]
async fn scale_create_type_junction_inserts_do_not_grow_with_parent_count() {
    // Allowed-parent/membership junction rows insert via a single
    // secure_insert_many call (RG-07).
    let small = junction_inserts_for_parent_count(2).await;
    let large = junction_inserts_for_parent_count(8).await;
    assert_eq!(
        small, large,
        "gts_type_allowed_parent INSERT count must not scale with \
         allowed_parent_types length (small={small} at N=2, large={large} at N=8)"
    );
}

async fn list_types_total_statements_for_page_size(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    // Self-referencing types (non-empty allowed_parent_types) so the batch
    // loader's junction-row and id->code resolution queries are actually
    // exercised for every row in the page, not skipped as trivially empty.
    for i in 0..n {
        create_self_referencing_type(&type_svc, &format!("listscale{i}")).await;
    }

    rec.clear();
    let query = toolkit_odata::ODataQuery {
        limit: Some(n as u64 + 5),
        ..Default::default()
    };
    let page = type_svc
        .list_types(&query)
        .await
        .expect("list_types should succeed");
    assert_eq!(page.items.len(), n, "page must contain all N created types");
    rec.total()
}

#[tokio::test]
async fn scale_list_types_statements_do_not_grow_with_page_size() {
    // load_full_types_batch issues a constant number of queries for the
    // whole page, regardless of page size (RG-12, the one read-path finding).
    let small = list_types_total_statements_for_page_size(3).await;
    let large = list_types_total_statements_for_page_size(15).await;
    assert_eq!(
        small, large,
        "list_types total statement count must not scale with page size \
         (small={small} at N=3, large={large} at N=15)"
    );
}

#[tokio::test]
async fn create_type_conflict_check_does_not_overfetch_junctions() {
    // resolve_id's existence check is a plain id lookup, with no junction
    // reads on either the happy or conflict path (RG-13).
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let t = common::create_root_type(&type_svc, "conflict").await;

    rec.clear();
    let result = type_svc
        .create_type(CreateTypeRequest {
            code: t.code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(resource_group::domain::error::DomainError::TypeAlreadyExists { .. })
        ),
        "expected a clean TypeAlreadyExists for the conflicting create, got: {result:?}"
    );

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
        "RG-13 regression: the duplicate-code conflict check must not read \
         gts_type_allowed_parent at all, got {parent_junction_selects} SELECTs:\n{}",
        rec.dump()
    );
    assert_eq!(
        membership_junction_selects,
        0,
        "RG-13 regression: the duplicate-code conflict check must not read \
         gts_type_allowed_membership at all, got {membership_junction_selects} SELECTs:\n{}",
        rec.dump()
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
/// Distinct types on purpose: the classifier has to learn each child's type
/// path to decide whether the child is nameable in the rejection, and a
/// per-type lookup is the shape that scales. Children sharing one type would
/// hide the defect behind the memoization the loop used to rely on.
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
/// or twelve of different types do. It used to resolve each distinct type
/// with its own `SELECT`, memoized per `gts_type_id` -- which bounded the
/// cost by the number of distinct types rather than removing the growth.
///
/// This case is the one the original audit did not have: nine scale tests
/// covered create, move, force delete, the two `$filter` paths and the type
/// operations, but nothing covered `delete_group(force = false)`, so the
/// regression could land unseen.
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

/// N+1 audit finding (b) guard: `gts_type` SELECTs for a `type in (...)`
/// `$filter` on `list_groups`, with `n` values in the list. No groups of
/// any of these types are created -- this isolates
/// `resolve_type_filter_node`'s own query cost from `resolve_type_paths_batch`'s
/// (which would otherwise also touch `gts_type` once per page, but is
/// skipped entirely when the page is empty).
async fn gts_type_selects_for_list_groups_type_in_filter(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let mut codes = Vec::with_capacity(n);
    for i in 0..n {
        let t = common::create_root_type(&type_svc, &format!("infiltn{i}")).await;
        codes.push(t.code);
    }

    rec.clear();
    let quoted: Vec<String> = codes.iter().map(|c| format!("'{c}'")).collect();
    let parsed = toolkit_odata::parse_filter_string(&format!("type in ({})", quoted.join(", ")))
        .expect("parse type in-list filter");
    let query = toolkit_odata::ODataQuery::new().with_filter(parsed.into_expr());

    let page = group_svc
        .list_groups(&ctx, &query)
        .await
        .expect("list_groups with a type in-list filter should succeed");
    assert_eq!(
        page.items.len(),
        0,
        "no groups were created of these types, only the types themselves"
    );

    count_in(&rec.stats(), QueryKind::Select, "gts_type")
}

#[tokio::test]
async fn scale_list_groups_type_in_filter_gts_type_selects_do_not_grow_with_value_count() {
    // resolve_type_filter_node batches every literal in a `type in (...)`
    // filter into one `WHERE schema_id IN (...)` query (N+1 audit finding
    // (b)) instead of one `resolve_id` round trip per value (pre-fix:
    // N=3 -> 4 gts_type SELECTs, N=20 -> 21, slope 1.0).
    let small = gts_type_selects_for_list_groups_type_in_filter(3).await;
    let large = gts_type_selects_for_list_groups_type_in_filter(20).await;
    assert_eq!(
        small, large,
        "gts_type SELECT count for a `type in (...)` filter on list_groups must \
         not scale with the number of values in the list (small={small} at N=3, \
         large={large} at N=20)"
    );
}

/// Same guard as the one above, but through `list_memberships`'s
/// `resource_type in (...)` filter -- `MembershipRepository::list_memberships`
/// calls the exact same `resolve_type_filter_node`, so
/// this is a second call site for the same fix, not a second
/// implementation of it.
async fn gts_type_selects_for_list_memberships_resource_type_in_filter(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let mut member_type_codes = Vec::with_capacity(n);
    for i in 0..n {
        let t = common::create_root_type(&type_svc, &format!("mifiltn{i}")).await;
        member_type_codes.push(t.code);
    }
    let grp_type = create_type_with_memberships(
        &type_svc,
        "mifiltgrp",
        &member_type_codes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
    .await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant_id).await;

    rec.clear();
    let quoted: Vec<String> = member_type_codes.iter().map(|c| format!("'{c}'")).collect();
    let parsed =
        toolkit_odata::parse_filter_string(&format!("resource_type in ({})", quoted.join(", ")))
            .expect("parse resource_type in-list filter");
    let query = toolkit_odata::ODataQuery::new().with_filter(parsed.into_expr());

    let page = membership_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list_memberships with a resource_type in-list filter should succeed");
    assert_eq!(
        page.items.len(),
        0,
        "no memberships were added -- {} exists only to make the group's type valid",
        group.id
    );

    count_in(&rec.stats(), QueryKind::Select, "gts_type")
}

#[tokio::test]
async fn scale_list_memberships_resource_type_in_filter_gts_type_selects_do_not_grow_with_value_count()
 {
    // Same fix as `scale_list_groups_type_in_filter_gts_type_selects_do_not_grow_with_value_count`,
    // exercised through the other call site of `resolve_type_filter_node`
    // (N+1 audit finding (b): this path was affected by the same defect,
    // and picked it up for free from the shared tree-walk generalization
    // in fe2d609e).
    let small = gts_type_selects_for_list_memberships_resource_type_in_filter(3).await;
    let large = gts_type_selects_for_list_memberships_resource_type_in_filter(20).await;
    assert_eq!(
        small, large,
        "gts_type SELECT count for a `resource_type in (...)` filter on \
         list_memberships must not scale with the number of values in the list \
         (small={small} at N=3, large={large} at N=20)"
    );
}

/// N+1 audit finding (b) guard, second half: `update_type`'s total
/// statement count when removing `n` allowed-parent-types at once, none of
/// which are actually in use by any group (so the safety check runs to
/// completion for all of them instead of early-returning on the first
/// violation).
async fn total_statements_for_update_type_removing_n_parents(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db.clone());

    let mut parent_codes = Vec::with_capacity(n);
    for i in 0..n {
        let t = common::create_root_type(&type_svc, &format!("rmpar{i}")).await;
        parent_codes.push(t.code);
    }
    let parent_refs: Vec<&str> = parent_codes.iter().map(String::as_str).collect();
    let child_type = common::create_child_type(&type_svc, "rmchild", &parent_refs, &[]).await;

    rec.clear();
    type_svc
        .update_type(
            &child_type.code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update_type removing all N allowed parents should succeed (unused by any group)");
    rec.total()
}

#[tokio::test]
async fn scale_update_type_removed_parents_statements_do_not_grow_with_count() {
    // check_hierarchy_safety batches the resolve + violating-group lookup
    // for every removed parent type into a small constant number of
    // queries (N+1 audit finding (b)) instead of one resolve_id + one
    // single-parent lookup *per* removed parent (pre-fix: N=3 -> 16 total,
    // N=20 -> 50, slope 2.0).
    let small = total_statements_for_update_type_removing_n_parents(3).await;
    let large = total_statements_for_update_type_removing_n_parents(20).await;
    assert_eq!(
        small, large,
        "update_type's total statement count when removing N allowed parent \
         types must not scale with N (small={small} at N=3, large={large} at N=20)"
    );
}

// Section 3 -- negative controls: both rely on SERIALIZABLE + retry and
// must show writes_outside_tx() == empty, proving the no-tx-write rule
// doesn't flag these paths.
//
// SQLite can't exercise the actual SSI conflict; see tests/pg_concurrency_test.rs.

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
        .create_type(CreateTypeRequest {
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
    type_svc
        .list_types(&toolkit_odata::ODataQuery::default())
        .await
        .expect("list_types should succeed");

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

// Section 4 -- static source-scan rules for two defect classes not
// observable as SQL: RG-03 (SERIALIZABLE without retry) and RG-09 (an
// external call inside a transaction closure), matched on call shape.

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn static_rule_passes_group_service_uses_retry() {
    let src = include_str!("../src/domain/group_service.rs");
    let unretried = count_occurrences(
        src,
        ".transaction_ref_mapped_with_config(TxConfig::serializable()",
    );
    let retried = count_occurrences(src, ".transaction_with_retry(TxConfig::serializable()");
    assert_eq!(
        unretried, 0,
        "negative control violated: group_service.rs should not bypass \
         transaction_with_retry for its SERIALIZABLE writes"
    );
    assert!(
        retried >= 3,
        "expected create/update/move/delete_group to all use \
         transaction_with_retry, found {retried}"
    );
}

// Section 5 -- contract-drift rules: a "contract" is a documented promise
// (DESIGN.md) about observable behavior; where the code doesn't yet keep
// it, the drift becomes an executable #[ignore]d assertion, not a comment.
//
// A third DESIGN.md promise (pool-level statement_timeout) is explicitly
// scoped as a deployment concern with no code path to assert against
// (checked toolkit_db::ConnectOpts in full), so it gets no test here.
