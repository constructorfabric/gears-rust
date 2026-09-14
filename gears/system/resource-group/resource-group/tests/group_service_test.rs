// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-entity-hierarchy:p1
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Phase 3 tests: Entity hierarchy operations.
//!
//! Covers TC-GRP-01..38, TC-META-12..18.
//! Group CRUD, parent-child with closure table verification, move with subtree
//! rebuild, cycle detection, type compatibility, query profile enforcement,
//! delete with reference checks, force cascade, hierarchy depth traversal,
//! and group metadata (barrier) storage and retrieval.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_gts::gts_id;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::metrics::{Operation, Outcome, RgMetricsPort};
use resource_group::domain::repo::GroupRepositoryTrait;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::gts_type::{
    Column as GtsTypeColumn, Entity as GtsTypeEntity,
};
use resource_group::infra::storage::entity::resource_group::{
    Column as RgColumn, Entity as RgEntity, Model as RgModel,
};
use resource_group::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::models::{ResourceGroup as ResourceGroupSdkModel, ResourceGroupWithDepth};
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, UpdateGroupRequest, UpdateTypeRequest,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;

/// Build a `GroupService` with custom `QueryProfile`.
fn make_group_service_with_profile(
    db: std::sync::Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
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

// =========================================================================
// Group creation tests (TC-GRP-01, 02, 03, 04, 22, 23, 24, 25)
// =========================================================================

/// TC-GRP-01: Create child group with parent -- closure rows.
/// Child has parent_id, closure: self(0) + ancestor(1).
#[tokio::test]
async fn group_create_child_with_closure() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create a root type and a child type that allows it as parent
    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    // Create root group
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Create child group
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    // Verify child fields
    assert_eq!(child.hierarchy.parent_id, Some(root.id));
    assert_eq!(child.hierarchy.tenant_id, tenant_id);
    assert_eq!(child.name, "Child");

    // Verify closure table: root has self-row only
    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, root.id, &[(root.id, 0)]).await;

    // Verify closure table: child has self-row + ancestor at depth 1
    common::assert_closure_rows(&conn, child.id, &[(child.id, 0), (root.id, 1)]).await;
}

/// TC-GRP-02: 3-level hierarchy -- closure completeness.
/// Child: grandparent(2), parent(1), self(0). Total 6 rows.
#[tokio::test]
async fn group_three_level_hierarchy_closure() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    // Grandchild type allows child_type as parent
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;
    let grandchild = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        child.id,
        "Grandchild",
        tenant_id,
    )
    .await;

    let conn = db.conn().expect("conn");

    // Root: self only
    common::assert_closure_rows(&conn, root.id, &[(root.id, 0)]).await;
    // Child: self + root at depth 1
    common::assert_closure_rows(&conn, child.id, &[(child.id, 0), (root.id, 1)]).await;
    // Grandchild: self + child(1) + root(2)
    common::assert_closure_rows(
        &conn,
        grandchild.id,
        &[(grandchild.id, 0), (child.id, 1), (root.id, 2)],
    )
    .await;

    // Total closure rows for all 3 groups = 1 + 2 + 3 = 6
    common::assert_closure_count(&conn, &[root.id, child.id, grandchild.id], 6).await;
}

/// TC-GRP-03: Create group with incompatible parent type.
#[tokio::test]
async fn group_create_incompatible_parent_type() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let other_root_type = common::create_root_type(&type_svc, "other").await;
    // unrelated_type allows only other_root_type as parent, NOT root_type
    let unrelated_type =
        common::create_child_type(&type_svc, "unrelated", &[&other_root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(unrelated_type.code.clone(), "Bad".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { .. }),
        "Expected InvalidParentType, got: {err:?}"
    );
}

/// TC-GRP-04: Create root when can_be_root=false.
#[tokio::test]
async fn group_create_root_when_cannot_be_root() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "Rootless".to_owned()),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { ref message } if message.contains("cannot be a root group")),
        "Expected InvalidParentType with 'cannot be a root group', got: {err:?}"
    );
}

/// TC-GRP-22: Create group with nonexistent type_path.
#[tokio::test]
async fn group_create_nonexistent_type() {
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(
                gts_id!("cf.core.rg.type.v1~x.test.nonexistent.type.v1~").to_owned(),
                "Ghost".to_owned(),
            ),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::TypeNotFound { .. }),
        "Expected TypeNotFound, got: {err:?}"
    );
}

/// TC-GRP-23: Child group cross-tenant parent.
#[tokio::test]
async fn group_create_cross_tenant_parent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = common::make_ctx(tenant_a);
    let ctx_b = common::make_ctx(tenant_b);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    // Create root under tenant A
    let root_a =
        common::create_root_group(&group_svc, &ctx_a, &root_type.code, "RootA", tenant_a).await;

    // Try to create child under tenant B with parent in tenant A
    let err = group_svc
        .create_group(
            &ctx_b,
            CreateGroupRequest::new(child_type.code.clone(), "CrossTenant".to_owned())
                .with_parent_id(Some(root_a.id)),
            tenant_b,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref message } if message.contains("must match parent tenant")),
        "Expected Validation with tenant mismatch, got: {err:?}"
    );
}

/// TC-GRP-24: Create group with metadata JSONB.
#[tokio::test]
async fn group_create_with_metadata() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"department": "engineering", "code": 42});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "WithMeta".to_owned())
                .with_metadata(Some(meta.clone())),
            tenant_id,
        )
        .await
        .expect("create group with metadata");

    assert_eq!(group.metadata, Some(meta.clone()));

    // Verify DB directly
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(group.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.metadata, Some(meta));
}

/// TC-GRP-25: Multiple root groups same type.
#[tokio::test]
async fn group_multiple_roots_same_type() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let root1 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root1", tenant_id).await;
    let root2 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root2", tenant_id).await;

    assert_ne!(root1.id, root2.id);
    assert_eq!(root1.code, root2.code);

    // Both have self-row closure only
    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, root1.id, &[(root1.id, 0)]).await;
    common::assert_closure_rows(&conn, root2.id, &[(root2.id, 0)]).await;
}

// =========================================================================
// Group move tests (TC-GRP-05, 06, 07, 08, 29, 30, 31, 32, 33)
// =========================================================================

/// TC-GRP-05: Move group -- closure rebuild.
/// Child.parent_id==Root2. Old paths to Root1 removed. New paths correct.
#[tokio::test]
async fn group_move_closure_rebuild() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let root1 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root1", tenant_id).await;
    let root2 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root2", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root1.id,
        "Child",
        tenant_id,
    )
    .await;
    let grandchild = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        child.id,
        "Grandchild",
        tenant_id,
    )
    .await;

    // Move child (and its subtree) from root1 to root2
    let moved = group_svc
        .move_group(child.id, Some(root2.id))
        .await
        .expect("move group");

    assert_eq!(moved.hierarchy.parent_id, Some(root2.id));

    let conn = db.conn().expect("conn");

    // Root1 untouched: still just self-row
    common::assert_closure_rows(&conn, root1.id, &[(root1.id, 0)]).await;

    // Root2 still just self-row
    common::assert_closure_rows(&conn, root2.id, &[(root2.id, 0)]).await;

    // Child: now has self + root2(1), no root1
    common::assert_closure_rows(&conn, child.id, &[(child.id, 0), (root2.id, 1)]).await;

    // Grandchild: self + child(1) + root2(2), no root1
    common::assert_closure_rows(
        &conn,
        grandchild.id,
        &[(grandchild.id, 0), (child.id, 1), (root2.id, 2)],
    )
    .await;

    // Verify entity state: parent_id changed, name and tenant_id unchanged
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(child.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.parent_id, Some(root2.id));
    assert_eq!(model.tenant_id, tenant_id);
    assert_eq!(model.name, "Child");
}

/// A type that allows itself as a parent, so a chain of arbitrary depth can
/// be built. `resolve_ids` rejects a parent path that does not exist yet, so
/// the self-reference is added by a follow-up update.
async fn create_self_parenting_type(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
) -> resource_group_sdk::ResourceGroupType {
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
        .expect("create self-parenting type");
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
        .expect("add the self-reference")
}

/// Move a two-level subtree under a parent that is itself two levels down.
///
/// `group_move_closure_rebuild` above moves under a *root*, so every new
/// closure row gets its depth from the subtree side alone. Here the new
/// parent has ancestors of its own, so each rebuilt row's depth is a sum of
/// both sides plus one -- the arithmetic that a rebuild can get wrong in a
/// way a move-to-root never exposes.
#[tokio::test]
async fn group_move_under_deep_parent_rebuilds_every_depth() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t = create_self_parenting_type(&type_svc, "deepmv").await;

    // Destination chain: root -> mid -> target. `target` sits at depth 2, so
    // it has three closure rows of its own (itself, mid, root).
    let root = common::create_root_group(&group_svc, &ctx, &t.code, "root", tenant_id).await;
    let mid =
        common::create_child_group(&group_svc, &ctx, &t.code, root.id, "mid", tenant_id).await;
    let target =
        common::create_child_group(&group_svc, &ctx, &t.code, mid.id, "target", tenant_id).await;

    // Subtree to move: other -> moved -> leaf.
    let other = common::create_root_group(&group_svc, &ctx, &t.code, "other", tenant_id).await;
    let moved =
        common::create_child_group(&group_svc, &ctx, &t.code, other.id, "moved", tenant_id).await;
    let leaf =
        common::create_child_group(&group_svc, &ctx, &t.code, moved.id, "leaf", tenant_id).await;

    group_svc
        .move_group(moved.id, Some(target.id))
        .await
        .expect("move under the deep parent");

    let conn = db.conn().expect("conn");

    // moved: self, target(1), mid(2), root(3) -- and nothing from `other`.
    common::assert_closure_rows(
        &conn,
        moved.id,
        &[(moved.id, 0), (target.id, 1), (mid.id, 2), (root.id, 3)],
    )
    .await;

    // leaf: one deeper on every path, and its link to `moved` is preserved
    // rather than rewritten -- internal subtree rows are not part of a move.
    common::assert_closure_rows(
        &conn,
        leaf.id,
        &[
            (leaf.id, 0),
            (moved.id, 1),
            (target.id, 2),
            (mid.id, 3),
            (root.id, 4),
        ],
    )
    .await;

    // The old parent keeps only itself.
    common::assert_closure_rows(&conn, other.id, &[(other.id, 0)]).await;

    common::assert_closure_matches_parent_links(&conn).await;
}

/// A move whose new parent belongs to another tenant is refused.
///
/// `tenant_id` is immutable gear-wide, so accepting the move would either
/// carry a group out of its tenant or leave it parented across the boundary.
/// Nothing covered this: the check could be deleted outright and the whole
/// suite stayed green.
#[tokio::test]
async fn group_move_to_parent_in_another_tenant_rejected() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let t = create_self_parenting_type(&type_svc, "xtenant").await;

    let tenant_a = Uuid::now_v7();
    let ctx_a = common::make_ctx(tenant_a);
    let tenant_b = Uuid::now_v7();
    let ctx_b = common::make_ctx(tenant_b);

    let root_a = common::create_root_group(&group_svc, &ctx_a, &t.code, "a-root", tenant_a).await;
    let child_a =
        common::create_child_group(&group_svc, &ctx_a, &t.code, root_a.id, "a-child", tenant_a)
            .await;
    let root_b = common::create_root_group(&group_svc, &ctx_b, &t.code, "b-root", tenant_b).await;

    let err = group_svc
        .move_group(child_a.id, Some(root_b.id))
        .await
        .expect_err("a move across the tenant boundary must be refused");

    let msg = err.to_string();
    assert!(
        msg.contains("different tenant"),
        "expected a cross-tenant rejection, got: {msg}"
    );
    // The caller cannot act on a foreign tenant id, and naming it would
    // disclose ownership of `root_b` across the boundary.
    assert!(
        !msg.contains(&tenant_b.to_string()),
        "the message names the foreign tenant: {msg}"
    );

    // The transaction rolled back: the child still hangs off its own root,
    // and the closure table still mirrors the parent links.
    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, child_a.id, &[(child_a.id, 0), (root_a.id, 1)]).await;
    common::assert_closure_matches_parent_links(&conn).await;
}

/// The same boundary, reached through `update_group`'s `parent_id` instead
/// of `move_group`.
///
/// `update_group_inner` carries its own copy of the check, and that copy was
/// the uncovered one: deleting it left the whole suite green.
#[tokio::test]
async fn group_update_parent_to_another_tenant_rejected() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let t = create_self_parenting_type(&type_svc, "xtenantupd").await;

    let tenant_a = Uuid::now_v7();
    let ctx_a = common::make_ctx(tenant_a);
    let tenant_b = Uuid::now_v7();
    let ctx_b = common::make_ctx(tenant_b);

    let root_a = common::create_root_group(&group_svc, &ctx_a, &t.code, "a-root", tenant_a).await;
    let child_a =
        common::create_child_group(&group_svc, &ctx_a, &t.code, root_a.id, "a-child", tenant_a)
            .await;
    let root_b = common::create_root_group(&group_svc, &ctx_b, &t.code, "b-root", tenant_b).await;

    let err = group_svc
        .update_group(
            &ctx_a,
            child_a.id,
            UpdateGroupRequest {
                name: "a-child".to_owned(),
                parent_id: Some(root_b.id),
                metadata: None,
            },
        )
        .await
        .expect_err("re-parenting across the tenant boundary must be refused");

    let msg = err.to_string();
    assert!(
        msg.contains("different tenant"),
        "expected a cross-tenant rejection, got: {msg}"
    );
    assert!(
        !msg.contains(&tenant_b.to_string()),
        "the message names the foreign tenant: {msg}"
    );

    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, child_a.id, &[(child_a.id, 0), (root_a.id, 1)]).await;
    common::assert_closure_matches_parent_links(&conn).await;
}

/// TC-GRP-06: Move under descendant -> CycleDetected.
#[tokio::test]
async fn group_move_under_descendant_cycle() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    // Try to move root under its child
    let err = group_svc
        .move_group(root.id, Some(child.id))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::CycleDetected { .. }),
        "Expected CycleDetected, got: {err:?}"
    );
}

/// TC-GRP-07: Self-parent -> CycleDetected.
#[tokio::test]
async fn group_move_self_parent_cycle() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    let err = group_svc
        .move_group(root.id, Some(root.id))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::CycleDetected { .. }),
        "Expected CycleDetected, got: {err:?}"
    );
}

/// TC-GRP-08: Move to incompatible parent type.
#[tokio::test]
async fn group_move_incompatible_parent_type() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let type_a = common::create_root_type(&type_svc, "orgA").await;
    let type_b = common::create_root_type(&type_svc, "orgB").await;
    // child type only allows type_a as parent
    let child_type = common::create_child_type(&type_svc, "dept", &[&type_a.code], &[]).await;

    let root_a =
        common::create_root_group(&group_svc, &ctx, &type_a.code, "RootA", tenant_id).await;
    let root_b =
        common::create_root_group(&group_svc, &ctx, &type_b.code, "RootB", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root_a.id,
        "Child",
        tenant_id,
    )
    .await;

    // Move child to root_b (incompatible)
    let err = group_svc
        .move_group(child.id, Some(root_b.id))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { .. }),
        "Expected InvalidParentType, got: {err:?}"
    );
}

/// TC-GRP-29: Move child to root (detach).
/// parent_id=None, closure rebuilt (self-row only).
#[tokio::test]
async fn group_move_child_to_root() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create a type that can be both root and child
    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_code = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.flexible{}.v1~",
        Uuid::now_v7().as_simple()
    );
    let _flexible_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![root_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create flexible type");

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child =
        common::create_child_group(&group_svc, &ctx, &child_code, root.id, "Child", tenant_id)
            .await;

    // Move child to root (detach from parent)
    let moved = group_svc
        .move_group(child.id, None)
        .await
        .expect("move to root");

    assert_eq!(moved.hierarchy.parent_id, None);

    let conn = db.conn().expect("conn");
    // Child should have only self-row now
    common::assert_closure_rows(&conn, child.id, &[(child.id, 0)]).await;
    common::assert_closure_count(&conn, &[child.id], 1).await;
}

/// TC-GRP-30: Move to root when can_be_root=false.
#[tokio::test]
async fn group_move_to_root_cannot_be_root() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    let err = group_svc.move_group(child.id, None).await.unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { ref message } if message.contains("cannot be a root group")),
        "Expected InvalidParentType with 'cannot be a root group', got: {err:?}"
    );
}

/// TC-GRP-31: Move nonexistent group.
#[tokio::test]
async fn group_move_nonexistent() {
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());

    let err = group_svc
        .move_group(Uuid::now_v7(), None)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "Expected GroupNotFound, got: {err:?}"
    );
}

/// TC-GRP-32: Move to nonexistent parent.
#[tokio::test]
async fn group_move_to_nonexistent_parent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    let err = group_svc
        .move_group(root.id, Some(Uuid::now_v7()))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "Expected GroupNotFound, got: {err:?}"
    );
}

/// TC-GRP-33: max_width enforcement on move.
#[tokio::test]
async fn group_move_max_width_exceeded() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let profile = QueryProfile {
        max_depth: None,
        max_width: Some(1),
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_code = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.flex{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![root_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create flexible child type");

    let root1 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root1", tenant_id).await;
    let root2 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root2", tenant_id).await;

    // Create one child under root1 (fills max_width=1)
    common::create_child_group(&group_svc, &ctx, &child_code, root1.id, "Child1", tenant_id).await;

    // Create a standalone group, then try to move it under root1
    let standalone =
        common::create_root_group(&group_svc, &ctx, &child_code, "Standalone", tenant_id).await;

    let err = group_svc
        .move_group(standalone.id, Some(root1.id))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Width limit exceeded")),
        "Expected LimitViolation with 'Width limit exceeded', got: {err:?}"
    );

    // Verify root2 is unaffected
    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, root2.id, &[(root2.id, 0)]).await;
}

// =========================================================================
// Group update tests (TC-GRP-09, 10, 11, 26, 27, 28)
// =========================================================================

/// TC-GRP-09: Update group name and metadata.
#[tokio::test]
async fn group_update_name_and_metadata() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "OldName", tenant_id).await;

    let new_meta = json!({"updated": true});
    let updated = group_svc
        .update_group(
            &ctx,
            root.id,
            UpdateGroupRequest {
                name: "NewName".to_owned(),
                parent_id: None,
                metadata: Some(new_meta.clone()),
            },
        )
        .await
        .expect("update group");

    assert_eq!(updated.name, "NewName");
    assert_eq!(updated.metadata, Some(new_meta.clone()));
    // parent_id and type unchanged
    assert_eq!(updated.hierarchy.parent_id, None);
    assert_eq!(updated.code, root_type.code);

    // Verify DB directly
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(root.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.name, "NewName");
    assert_eq!(model.metadata, Some(new_meta));
}

// Removed: TC-GRP-10 (`group_update_type_parent_incompatible`) and TC-GRP-11
// (`group_update_type_children_incompatible`) were authored when
// `UpdateGroupRequest` carried a `code` field. Now that the group's GTS type
// is immutable post-creation (per DESIGN: "The group's type is immutable
// after creation"), these scenarios are physically unreachable through the
// SDK — `update_group` cannot trigger a parent/children type-compatibility
// failure because the type never changes. Coverage of the underlying
// invariant lives in the `create_group` and `move_group` paths instead.

// Removed: TC-GRP-26 (`group_update_simultaneous_type_and_parent`),
// TC-GRP-27 (`group_update_root_to_nonroot_type`), TC-GRP-28
// (`group_update_nonexistent_type`) — the same reason as TC-GRP-10/11
// above. All three exercised the now-impossible "type changes via
// `update_group`" scenario; with `UpdateGroupRequest` carrying only
// `name` / `parent_id` / `metadata`, none of these cases are
// physically reachable. Parent change in isolation is already
// covered by TC-GRP-09 (`group_update`).

// =========================================================================
// Group delete tests (TC-GRP-12, 13, 14, 15, 34, 35)
// =========================================================================

/// TC-GRP-12: Delete leaf group.
/// Success, no group, closure rows removed.
#[tokio::test]
async fn group_delete_leaf() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    // Delete the child (leaf)
    group_svc
        .delete_group(&ctx, child.id, false)
        .await
        .expect("delete leaf");

    let conn = db.conn().expect("conn");

    // Child's closure rows gone
    common::assert_closure_count(&conn, &[child.id], 0).await;

    // Group entity gone
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(child.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query");
    assert!(model.is_none(), "Group should be deleted");

    // Parent's closure untouched
    common::assert_closure_rows(&conn, root.id, &[(root.id, 0)]).await;
}

/// TC-GRP-13: Delete with children no force.
#[tokio::test]
async fn group_delete_with_children_no_force() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let _child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    let err = group_svc
        .delete_group(&ctx, root.id, false)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::ConflictActiveReferences { ref message } if message.contains("child group(s)")),
        "Expected ConflictActiveReferences with 'child group(s)', got: {err:?}"
    );
}

/// TC-GRP-14: Delete with memberships no force.
/// Insert membership rows directly via SeaORM.
#[tokio::test]
async fn group_delete_with_memberships_no_force() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Insert membership directly. Resolve the surrogate `gts_type_id` from the
    // type we just created instead of hard-coding `1` — that hard-code would
    // silently break if `common::test_db()` ever seeds base types or if the
    // SMALLINT IDENTITY sequence behaviour changes.
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let root_type_id = GtsTypeEntity::find()
        .filter(GtsTypeColumn::SchemaId.eq(&root_type.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query gts_type")
        .expect("type row exists")
        .id;
    let membership = membership_entity::ActiveModel {
        group_id: Set(root.id),
        gts_type_id: Set(root_type_id),
        resource_id: Set("resource-1".to_owned()),
        ..Default::default()
    };
    secure_insert::<MembershipEntity>(membership, &scope, &conn)
        .await
        .expect("insert membership");

    let err = group_svc
        .delete_group(&ctx, root.id, false)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::ConflictActiveReferences { ref message } if message.contains("memberships")),
        "Expected ConflictActiveReferences with 'memberships', got: {err:?}"
    );
}

/// TC-GRP-15: Force delete subtree.
/// All 3 groups + memberships + closure gone.
#[tokio::test]
async fn group_force_delete_subtree() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;
    let grandchild = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        child.id,
        "Grandchild",
        tenant_id,
    )
    .await;

    // Add a membership to child (direct insert). Resolve the surrogate
    // `gts_type_id` from the actual type row instead of hard-coding `1`.
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let root_type_id = GtsTypeEntity::find()
        .filter(GtsTypeColumn::SchemaId.eq(&root_type.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query gts_type")
        .expect("type row exists")
        .id;
    let membership = membership_entity::ActiveModel {
        group_id: Set(child.id),
        gts_type_id: Set(root_type_id),
        resource_id: Set("resource-m".to_owned()),
        ..Default::default()
    };
    secure_insert::<MembershipEntity>(membership, &scope, &conn)
        .await
        .expect("insert membership");

    // Force delete root subtree
    group_svc
        .delete_group(&ctx, root.id, true)
        .await
        .expect("force delete");

    // All 3 groups gone
    for gid in &[root.id, child.id, grandchild.id] {
        let model = RgEntity::find()
            .filter(RgColumn::Id.eq(*gid))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await
            .expect("query");
        assert!(model.is_none(), "Group {gid} should be deleted");
    }

    // All closure rows gone
    common::assert_closure_count(&conn, &[root.id, child.id, grandchild.id], 0).await;

    // Memberships gone
    let mem_count = MembershipEntity::find()
        .filter(membership_entity::Column::GroupId.eq(child.id))
        .secure()
        .scope_with(&scope)
        .count(&conn)
        .await
        .expect("query memberships");
    assert_eq!(mem_count, 0, "Memberships should be deleted");
}

/// TC-GRP-34: Delete nonexistent group.
#[tokio::test]
async fn group_delete_nonexistent() {
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let err = group_svc
        .delete_group(&ctx, Uuid::now_v7(), false)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "Expected GroupNotFound, got: {err:?}"
    );
}

/// TC-GRP-35: Force delete leaf (no descendants).
#[tokio::test]
async fn group_force_delete_leaf() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    group_svc
        .delete_group(&ctx, root.id, true)
        .await
        .expect("force delete leaf");

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(root.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query");
    assert!(model.is_none(), "Group should be deleted");
    common::assert_closure_count(&conn, &[root.id], 0).await;
}

// =========================================================================
// Hierarchy endpoint tests (TC-GRP-16, 36)
// =========================================================================

/// TC-GRP-16: Hierarchy endpoint depth traversal.
/// A(depth=-1), B(depth=0), C(depth=1).
#[tokio::test]
async fn group_hierarchy_depth_traversal() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let a = common::create_root_group(&group_svc, &ctx, &root_type.code, "A", tenant_id).await;
    let b =
        common::create_child_group(&group_svc, &ctx, &child_type.code, a.id, "B", tenant_id).await;
    let c = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        b.id,
        "C",
        tenant_id,
    )
    .await;

    let query = ODataQuery::default();

    // Descendants of B: should return B (depth=0) and C (depth=1)
    let desc_page = group_svc
        .get_group_descendants(&ctx, b.id, &query)
        .await
        .expect("get descendants");
    assert_eq!(desc_page.items.len(), 2, "Descendants should return B, C");
    let item_b = desc_page
        .items
        .iter()
        .find(|i| i.id == b.id)
        .expect("B present");
    let item_c = desc_page
        .items
        .iter()
        .find(|i| i.id == c.id)
        .expect("C present");
    assert_eq!(item_b.hierarchy.depth, 0, "B should be at depth 0");
    assert_eq!(item_c.hierarchy.depth, 1, "C should be at depth 1");

    // Ancestors of B: should return B (depth=0) and A (depth=-1)
    let anc_page = group_svc
        .get_group_ancestors(&ctx, b.id, &query)
        .await
        .expect("get ancestors");
    assert_eq!(anc_page.items.len(), 2, "Ancestors should return A, B");
    let item_a = anc_page
        .items
        .iter()
        .find(|i| i.id == a.id)
        .expect("A present");
    let item_b = anc_page
        .items
        .iter()
        .find(|i| i.id == b.id)
        .expect("B present in ancestors");
    assert_eq!(item_a.hierarchy.depth, -1, "A should be at depth -1");
    assert_eq!(item_b.hierarchy.depth, 0, "B should be at depth 0");

    // All nodes have tenant_id and parent_id
    assert_eq!(item_a.hierarchy.tenant_id, tenant_id);
    assert_eq!(item_b.hierarchy.tenant_id, tenant_id);
    assert_eq!(item_c.hierarchy.tenant_id, tenant_id);
    assert_eq!(item_a.hierarchy.parent_id, None);
    assert_eq!(item_b.hierarchy.parent_id, Some(a.id));
    assert_eq!(item_c.hierarchy.parent_id, Some(b.id));
}

/// TC-GRP-36: get_group_descendants nonexistent group.
#[tokio::test]
async fn group_hierarchy_nonexistent() {
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let err = group_svc
        .get_group_descendants(&ctx, Uuid::now_v7(), &ODataQuery::default())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "Expected GroupNotFound, got: {err:?}"
    );
}

// =========================================================================
// Query profile tests (TC-GRP-17, 18, 19, 37, 38)
// =========================================================================

/// TC-GRP-17: max_depth enforcement on create.
#[tokio::test]
async fn group_create_max_depth_exceeded() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let profile = QueryProfile {
        max_depth: Some(1), // only root allowed (depth 0), child at depth 1 is >= max
        max_width: None,
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "TooDeep".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Depth limit exceeded")),
        "Expected LimitViolation with 'Depth limit exceeded', got: {err:?}"
    );
}

/// TC-GRP-18: max_width enforcement on create.
#[tokio::test]
async fn group_create_max_width_exceeded() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let profile = QueryProfile {
        max_depth: None,
        max_width: Some(1),
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // First child ok
    common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child1",
        tenant_id,
    )
    .await;

    // Second child exceeds max_width
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "Child2".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Width limit exceeded")),
        "Expected LimitViolation with 'Width limit exceeded', got: {err:?}"
    );
}

/// TC-GRP-19: max_depth enforcement on move.
#[tokio::test]
async fn group_move_max_depth_exceeded() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    // max_depth=2: root(0), child(1) ok, but grandchild(2) would be >= max
    let profile = QueryProfile {
        max_depth: Some(2),
        max_width: None,
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    // child_type allows root_type as parent, can also be root
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    // sub_type allows child_type as parent, can also be root
    let sub_code = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.sub{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: sub_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![child_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create sub type");

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    // Create a standalone root with a sub-child (standalone -> sub)
    // standalone is child_type (can be root=false, but we need it as root -- use sub_code which can be root)
    let standalone =
        common::create_root_group(&group_svc, &ctx, &sub_code, "Standalone", tenant_id).await;
    // sub needs a type that allows sub_code as parent -- create another type for that
    let subsub_code = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.subsub{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: subsub_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![sub_code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create subsub type");

    let _sub = common::create_child_group(
        &group_svc,
        &ctx,
        &subsub_code,
        standalone.id,
        "Sub",
        tenant_id,
    )
    .await;

    // Try to move standalone under child: standalone would be at depth 2, sub at depth 3
    // max_depth=2, so deepest = 1+1+1 = 3 >= 2 triggers violation
    // But standalone's type (sub_code) must allow child_type as parent.
    // Actually sub_code allows child_type as parent, so the move is type-compatible.
    let err = group_svc
        .move_group(standalone.id, Some(child.id))
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Depth limit")),
        "Expected LimitViolation, got: {err:?}"
    );
}

/// TC-GRP-37: Depth exact boundary (parent_depth+1 == max_depth).
/// LimitViolation (>= comparison).
#[tokio::test]
async fn group_create_depth_exact_boundary() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    // max_depth=2: root is at depth 0, child at depth 1 (parent_depth=0, 0+1=1 < 2 ok)
    // grandchild at depth 2 (parent_depth=1, 1+1=2 >= 2 -> violation)
    let profile = QueryProfile {
        max_depth: Some(2),
        max_width: None,
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let child = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child",
        tenant_id,
    )
    .await;

    // Grandchild at depth 2 should trigger exact boundary violation
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(grandchild_type.code.clone(), "Grandchild".to_owned())
                .with_parent_id(Some(child.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Depth limit exceeded")),
        "Expected LimitViolation at exact boundary, got: {err:?}"
    );
}

/// TC-GRP-38: Width exact boundary (sibling_count == max_width).
#[tokio::test]
async fn group_create_width_exact_boundary() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let profile = QueryProfile {
        max_depth: None,
        max_width: Some(2),
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Fill to max_width=2
    common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child1",
        tenant_id,
    )
    .await;
    common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        root.id,
        "Child2",
        tenant_id,
    )
    .await;

    // Third child triggers exact boundary (count=2 >= max_width=2)
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "Child3".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::LimitViolation { ref message } if message.contains("Width limit exceeded")),
        "Expected LimitViolation at exact boundary, got: {err:?}"
    );
}

// =========================================================================
// Name validation tests (TC-GRP-20, 21)
// =========================================================================

/// TC-GRP-20: Group name empty.
#[tokio::test]
async fn group_create_name_empty() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), String::new()),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref message } if message.contains("between 1 and 255")),
        "Expected Validation with 'between 1 and 255', got: {err:?}"
    );
}

/// TC-GRP-21: Group name >255 chars.
#[tokio::test]
async fn group_create_name_too_long() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let long_name = "x".repeat(256);
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), long_name),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref message } if message.contains("between 1 and 255")),
        "Expected Validation with 'between 1 and 255', got: {err:?}"
    );
}

// =========================================================================
// Metadata tests (TC-META-12..18)
// =========================================================================

/// TC-META-12: Group with metadata self_managed stored/returned.
/// metadata.self_managed == true, DB JSONB matches.
#[tokio::test]
async fn group_metadata_barrier_stored() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"self_managed": true});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "BarrierGroup".to_owned())
                .with_metadata(Some(meta.clone())),
            tenant_id,
        )
        .await
        .expect("create barrier group");

    assert_eq!(group.metadata.as_ref().unwrap()["self_managed"], true);

    // Verify DB directly
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(group.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.metadata, Some(meta));
}

/// TC-META-13: Group with rich metadata -- multiple fields.
/// All fields preserved.
#[tokio::test]
async fn group_metadata_rich_multiple_fields() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({
        "barrier": false,
        "region": "eu-west-1",
        "tags": ["prod", "critical"],
        "nested": {"level": 2, "active": true}
    });
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "RichMeta".to_owned())
                .with_metadata(Some(meta.clone())),
            tenant_id,
        )
        .await
        .expect("create rich metadata group");

    assert_eq!(group.metadata, Some(meta));
}

/// TC-META-14: Group metadata update replaces entirely (not merge).
/// Old keys gone.
#[tokio::test]
async fn group_metadata_update_replaces_entirely() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let old_meta = json!({"old_key": "old_value", "shared": 1});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "ReplaceMe".to_owned())
                .with_metadata(Some(old_meta)),
            tenant_id,
        )
        .await
        .expect("create group");

    let new_meta = json!({"new_key": "new_value"});
    let updated = group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "ReplaceMe".to_owned(),
                parent_id: None,
                metadata: Some(new_meta.clone()),
            },
        )
        .await
        .expect("update group");

    assert_eq!(updated.metadata, Some(new_meta.clone()));
    // Old key gone
    assert!(updated.metadata.as_ref().unwrap().get("old_key").is_none());

    // Verify DB directly
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(group.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.metadata, Some(new_meta));
}

/// TC-META-15: Group metadata None -> update with metadata.
/// Returns new metadata.
#[tokio::test]
async fn group_metadata_none_to_some() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "NoMeta".to_owned()),
            tenant_id,
        )
        .await
        .expect("create group");

    assert!(group.metadata.is_none());

    let meta = json!({"added": true});
    let updated = group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "NoMeta".to_owned(),
                parent_id: None,
                metadata: Some(meta.clone()),
            },
        )
        .await
        .expect("update group");

    assert_eq!(updated.metadata, Some(meta));
}

/// TC-META-16: Group metadata set -> update with None.
/// Metadata gone.
#[tokio::test]
async fn group_metadata_some_to_none() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"initial": true});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "WithMeta".to_owned())
                .with_metadata(Some(meta)),
            tenant_id,
        )
        .await
        .expect("create group");

    let updated = group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "WithMeta".to_owned(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .expect("update group");

    assert!(updated.metadata.is_none(), "Metadata should be cleared");
}

/// TC-META-17: Barrier group visible in hierarchy.
/// All 3 groups returned including barrier.
#[tokio::test]
async fn group_metadata_barrier_in_hierarchy() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    let grandchild_type =
        common::create_child_type(&type_svc, "team", &[&child_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Child is a barrier group
    let barrier = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "BarrierChild".to_owned())
                .with_parent_id(Some(root.id))
                .with_metadata(Some(json!({"self_managed": true}))),
            tenant_id,
        )
        .await
        .expect("create barrier child");

    let _leaf = common::create_child_group(
        &group_svc,
        &ctx,
        &grandchild_type.code,
        barrier.id,
        "Leaf",
        tenant_id,
    )
    .await;

    // Query descendants from root — should include root, barrier, leaf
    let query = ODataQuery::default();
    let page = group_svc
        .get_group_descendants(&ctx, root.id, &query)
        .await
        .expect("get descendants");

    assert_eq!(
        page.items.len(),
        3,
        "All 3 groups returned including barrier"
    );

    // Verify barrier is present as a descendant of root
    let barrier_item = page
        .items
        .iter()
        .find(|i| i.id == barrier.id)
        .expect("barrier present");
    assert_eq!(barrier_item.hierarchy.depth, 1, "barrier is child of root");
}

/// TC-META-18: Group metadata in hierarchy endpoint response.
/// Each GroupWithDepthDto has metadata.
#[tokio::test]
async fn group_metadata_in_hierarchy_response() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root_meta = json!({"level": "root"});
    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "Root".to_owned())
                .with_metadata(Some(root_meta.clone())),
            tenant_id,
        )
        .await
        .expect("create root");

    let child_meta = json!({"level": "child", "barrier": false});
    let child = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code.clone(), "Child".to_owned())
                .with_parent_id(Some(root.id))
                .with_metadata(Some(child_meta.clone())),
            tenant_id,
        )
        .await
        .expect("create child");

    let query = ODataQuery::default();
    let page = group_svc
        .get_group_descendants(&ctx, root.id, &query)
        .await
        .expect("get descendants");

    let root_item = page
        .items
        .iter()
        .find(|i| i.id == root.id)
        .expect("root present");
    let child_item = page
        .items
        .iter()
        .find(|i| i.id == child.id)
        .expect("child present");

    assert_eq!(root_item.metadata, Some(root_meta));
    assert_eq!(child_item.metadata, Some(child_meta));
}

// =========================================================================
// ADR-001 Hierarchy Reproduction (TC-ADR-01..08)
// =========================================================================

/// Helper: build the ADR-001 type ecosystem.
/// Returns (tenant_type, dept_type, branch_type, user_type, course_type).
async fn create_adr_types(
    type_svc: &resource_group::domain::type_service::TypeService<TypeRepository>,
) -> (
    resource_group_sdk::ResourceGroupType,
    resource_group_sdk::ResourceGroupType,
    resource_group_sdk::ResourceGroupType,
    resource_group_sdk::ResourceGroupType,
    resource_group_sdk::ResourceGroupType,
) {
    let user_type = common::create_root_type(type_svc, "adruser").await;
    let course_type = common::create_root_type(type_svc, "adrcourse").await;

    let suffix_t = format!("adrtenant{}", uuid::Uuid::now_v7().as_simple());
    let tenant_code = format!("{}x.test.{suffix_t}.v1~", gts_id!("cf.core.rg.type.v1~"));

    // Tenant type: create first without self-reference, then update
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: tenant_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![user_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create tenant type");

    let tenant_type = type_svc
        .update_type_unscoped(
            &tenant_code,
            resource_group_sdk::UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![tenant_code.clone()],
                allowed_membership_types: vec![user_type.code.clone()],
                metadata_schema: None,
            },
        )
        .await
        .expect("update tenant type with self-reference");

    // Dept type: NOT root, parent=tenant, allows users+courses
    let dept_type = common::create_child_type(
        type_svc,
        "adrdept",
        &[&tenant_type.code],
        &[&user_type.code, &course_type.code],
    )
    .await;

    // Branch type: NOT root, parent=dept, allows users+courses
    let branch_type = common::create_child_type(
        type_svc,
        "adrbranch",
        &[&dept_type.code],
        &[&user_type.code, &course_type.code],
    )
    .await;

    (tenant_type, dept_type, branch_type, user_type, course_type)
}

/// TC-ADR-01: Full ADR hierarchy reproduction.
/// Creates T1, D2, B3, T7, D8, T9 with types + memberships.
#[tokio::test]
async fn adr_full_hierarchy_reproduction() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, dept_type, branch_type, user_type, course_type) =
        create_adr_types(&type_svc).await;

    // T1: root tenant
    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;
    // D2: dept under T1
    let d2 =
        common::create_child_group(&group_svc, &ctx, &dept_type.code, t1.id, "D2", tenant_id).await;
    // B3: branch under D2
    let b3 =
        common::create_child_group(&group_svc, &ctx, &branch_type.code, d2.id, "B3", tenant_id)
            .await;
    // T7: tenant under T1 (self-nesting)
    let t7 =
        common::create_child_group(&group_svc, &ctx, &tenant_type.code, t1.id, "T7", tenant_id)
            .await;
    // D8: dept under T7
    let d8 =
        common::create_child_group(&group_svc, &ctx, &dept_type.code, t7.id, "D8", tenant_id).await;
    // T9: root tenant (independent)
    let t9 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T9", tenant_id).await;

    // Verify hierarchy positions
    assert!(t1.hierarchy.parent_id.is_none());
    assert_eq!(d2.hierarchy.parent_id, Some(t1.id));
    assert_eq!(b3.hierarchy.parent_id, Some(d2.id));
    assert_eq!(t7.hierarchy.parent_id, Some(t1.id));
    assert_eq!(d8.hierarchy.parent_id, Some(t7.id));
    assert!(t9.hierarchy.parent_id.is_none());

    // Verify closure table depths
    let conn = db.conn().expect("conn");
    // T1: self(0)
    common::assert_closure_rows(&conn, t1.id, &[(t1.id, 0)]).await;
    // D2: self(0), T1(1)
    common::assert_closure_rows(&conn, d2.id, &[(d2.id, 0), (t1.id, 1)]).await;
    // B3: self(0), D2(1), T1(2)
    common::assert_closure_rows(&conn, b3.id, &[(b3.id, 0), (d2.id, 1), (t1.id, 2)]).await;
    // T7: self(0), T1(1)
    common::assert_closure_rows(&conn, t7.id, &[(t7.id, 0), (t1.id, 1)]).await;
    // D8: self(0), T7(1), T1(2)
    common::assert_closure_rows(&conn, d8.id, &[(d8.id, 0), (t7.id, 1), (t1.id, 2)]).await;
    // T9: self(0)
    common::assert_closure_rows(&conn, t9.id, &[(t9.id, 0)]).await;

    // Add memberships: user R4 in T1, course R5 in B3, user R6 in D2
    membership_svc
        .add_membership(&ctx, t1.id, &user_type.code, "R4")
        .await
        .expect("add R4 user to T1");
    membership_svc
        .add_membership(&ctx, b3.id, &course_type.code, "R5")
        .await
        .expect("add R5 course to B3");
    membership_svc
        .add_membership(&ctx, d2.id, &user_type.code, "R6")
        .await
        .expect("add R6 user to D2");

    // Total closure rows: 1 + 2 + 3 + 2 + 3 + 1 = 12
    common::assert_closure_count(&conn, &[t1.id, d2.id, b3.id, t7.id, d8.id, t9.id], 12).await;
}

/// TC-ADR-02: Tenant allows self-nesting (T7 under T1).
#[tokio::test]
async fn adr_tenant_self_nesting() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, _, _, _, _) = create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;
    let t7 =
        common::create_child_group(&group_svc, &ctx, &tenant_type.code, t1.id, "T7", tenant_id)
            .await;
    assert_eq!(t7.hierarchy.parent_id, Some(t1.id));
}

/// TC-ADR-03: Department cannot be root.
#[tokio::test]
async fn adr_department_cannot_be_root() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (_, dept_type, _, _, _) = create_adr_types(&type_svc).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(dept_type.code.clone(), "RootDept".to_owned()),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { .. }),
        "Expected InvalidParentType, got {err:?}"
    );
}

/// TC-ADR-04: Branch only under department -- fails under tenant.
#[tokio::test]
async fn adr_branch_only_under_department() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, _, branch_type, _, _) = create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(branch_type.code.clone(), "BadBranch".to_owned())
                .with_parent_id(Some(t1.id)),
            tenant_id,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            DomainError::InvalidParentType { .. } | DomainError::AllowedParentTypesViolation { .. }
        ),
        "Expected InvalidParentType or AllowedParentTypesViolation, got {err:?}"
    );
}

/// TC-ADR-05: Branch allows users AND courses memberships.
#[tokio::test]
async fn adr_branch_allows_users_and_courses() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, dept_type, branch_type, user_type, course_type) =
        create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;
    let d2 =
        common::create_child_group(&group_svc, &ctx, &dept_type.code, t1.id, "D2", tenant_id).await;
    let b3 =
        common::create_child_group(&group_svc, &ctx, &branch_type.code, d2.id, "B3", tenant_id)
            .await;

    // Both should succeed
    membership_svc
        .add_membership(&ctx, b3.id, &user_type.code, "user-1")
        .await
        .expect("add user to branch");
    membership_svc
        .add_membership(&ctx, b3.id, &course_type.code, "course-1")
        .await
        .expect("add course to branch");
}

/// TC-ADR-06: Tenant allows only users (not courses).
#[tokio::test]
async fn adr_tenant_rejects_course_membership() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, _, _, _, course_type) = create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;

    let err = membership_svc
        .add_membership(&ctx, t1.id, &course_type.code, "course-bad")
        .await
        .unwrap_err();

    assert!(
        matches!(
            &err,
            DomainError::Validation { message } if message.contains("allowed_membership_types")
        ),
        "Expected DomainError::Validation mentioning allowed_membership_types, got: {err:?}"
    );
}

/// TC-ADR-07: Same user in multiple groups (D8 + T7).
#[tokio::test]
async fn adr_same_user_in_multiple_groups() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, dept_type, _, user_type, _) = create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;
    let t7 =
        common::create_child_group(&group_svc, &ctx, &tenant_type.code, t1.id, "T7", tenant_id)
            .await;
    let d8 =
        common::create_child_group(&group_svc, &ctx, &dept_type.code, t7.id, "D8", tenant_id).await;

    // Same user in both groups
    membership_svc
        .add_membership(&ctx, t7.id, &user_type.code, "shared-user")
        .await
        .expect("add user to T7");
    membership_svc
        .add_membership(&ctx, d8.id, &user_type.code, "shared-user")
        .await
        .expect("add same user to D8");
}

/// TC-ADR-08: Same resource different types (R4 as course in B3 + user in T1).
#[tokio::test]
async fn adr_same_resource_different_types() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let membership_svc = common::make_membership_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, dept_type, branch_type, user_type, course_type) =
        create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;
    let d2 =
        common::create_child_group(&group_svc, &ctx, &dept_type.code, t1.id, "D2", tenant_id).await;
    let b3 =
        common::create_child_group(&group_svc, &ctx, &branch_type.code, d2.id, "B3", tenant_id)
            .await;

    // R4 as course in B3
    membership_svc
        .add_membership(&ctx, b3.id, &course_type.code, "R4")
        .await
        .expect("add R4 as course to B3");
    // R4 as user in T1
    membership_svc
        .add_membership(&ctx, t1.id, &user_type.code, "R4")
        .await
        .expect("add R4 as user to T1");
}

// =========================================================================
// Security/Attack Tests for Group Metadata (TC-META-ATK-08, 09)
// =========================================================================

/// TC-META-ATK-08: SQL injection in group metadata is stored as-is, no injection.
#[tokio::test]
async fn security_group_metadata_sql_injection() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "sqlmeta").await;

    let evil_meta = json!({
        "name": "'; DROP TABLE resource_group; --",
        "value": "1 OR 1=1",
        "__internal": "attack"
    });

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "SQLMetaGroup".to_owned())
                .with_metadata(Some(evil_meta.clone())),
            tenant_id,
        )
        .await
        .expect("create group with SQL injection metadata");

    // Verify metadata stored as-is
    let loaded = group_svc
        .get_group(&ctx, group.id)
        .await
        .expect("get group");
    assert_eq!(loaded.metadata, Some(evil_meta));

    // Verify DB still works (table not dropped)
    let query = ODataQuery::default();
    let page = group_svc.list_groups(&ctx, &query).await;
    assert!(
        page.is_ok(),
        "DB should still work after SQL injection metadata"
    );
}

/// TC-META-ATK-09: Large metadata payload (1MB). Document behavior.
#[tokio::test]
async fn security_group_metadata_large_payload() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "bigmeta").await;

    let big_value = "A".repeat(1_000_000);
    let big_meta = json!({"payload": big_value});

    // Document behavior: SQLite may accept or reject based on limits.
    // The test verifies no panic occurs.
    let result = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "BigMetaGroup".to_owned())
                .with_metadata(Some(big_meta.clone())),
            tenant_id,
        )
        .await;

    match result {
        Ok(group) => {
            // If accepted, verify roundtrip
            let loaded = group_svc
                .get_group(&ctx, group.id)
                .await
                .expect("get group");
            assert_eq!(
                loaded.metadata.as_ref().unwrap()["payload"]
                    .as_str()
                    .unwrap()
                    .len(),
                1_000_000,
                "1MB payload should roundtrip"
            );
        }
        // Deterministic deny classes are acceptable: validation rejects oversize
        // payloads up-front, and the storage layer may reject through the DB
        // (e.g. SQLite parameter-size limits). Any other error class indicates
        // a regression.
        Err(DomainError::Validation { .. } | DomainError::Database(_)) => {}
        Err(e) => panic!("unexpected error class for large metadata payload: {e:?}"),
    }
}

// =========================================================================
// Tenant-root uniqueness (cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness)
// =========================================================================

/// Build a unique tenant-type code: code starts with `TENANT_RG_TYPE_PATH` so
/// `type_code.starts_with(TENANT_RG_TYPE_PATH)` classifies the group as a
/// tenant-type group.
fn unique_tenant_type_code() -> String {
    format!(
        "{}test{}.v1~",
        resource_group_sdk::TENANT_RG_TYPE_PATH,
        Uuid::now_v7().as_simple()
    )
}

/// Create a tenant-type RG type (`can_be_root=true`, `allowed_parent_types=[self]`).
async fn create_tenant_type(
    svc: &TypeService<TypeRepository>,
) -> resource_group_sdk::models::ResourceGroupType {
    // `allowed_parent_types = []` because self-references aren't allowed at
    // create time (the type is not yet in the registry). Suitable for testing
    // the uniqueness invariant at root level.
    svc.create_type_unscoped(resource_group_sdk::CreateTypeRequest {
        code: unique_tenant_type_code(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    })
    .await
    .expect("create tenant type")
}

/// Create a tenant-type RG type that allows being placed under the given
/// parent tenant-type (used to build a root→sub-tenant fixture).
async fn create_tenant_sub_type(
    svc: &TypeService<TypeRepository>,
    parent_type_code: &str,
) -> resource_group_sdk::models::ResourceGroupType {
    svc.create_type_unscoped(resource_group_sdk::CreateTypeRequest {
        code: unique_tenant_type_code(),
        can_be_root: true,
        allowed_parent_types: vec![parent_type_code.to_owned()],
        allowed_membership_types: vec![],
        metadata_schema: None,
    })
    .await
    .expect("create tenant sub-type")
}

/// TC-TRU-01: First tenant-type root is accepted.
#[tokio::test]
async fn tenant_root_first_create_allowed() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let tenant_type = create_tenant_type(&type_svc).await;
    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(tenant_type.code.clone(), "MainTenant".to_owned()),
            tenant_id,
        )
        .await
        .expect("first tenant root should succeed");
    assert!(root.hierarchy.parent_id.is_none());
    // Effective tenant_id = group.id for tenant-type groups.
    assert_eq!(root.hierarchy.tenant_id, root.id);
}

/// TC-TRU-02: Second tenant-type root is rejected with `TenantRootAlreadyExists`.
#[tokio::test]
async fn tenant_root_second_create_rejected() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create the first tenant root.
    let tenant_type = create_tenant_type(&type_svc).await;
    group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(tenant_type.code.clone(), "First".to_owned()),
            tenant_id,
        )
        .await
        .expect("first tenant root should succeed");

    // Second tenant-type root must be rejected regardless of type identity
    // (any tenant-type root collides with any other tenant-type root).
    let second_type = create_tenant_type(&type_svc).await;
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(second_type.code.clone(), "Second".to_owned()),
            Uuid::now_v7(),
        )
        .await
        .expect_err("second tenant root must be rejected");
    assert!(
        matches!(err, DomainError::TenantRootAlreadyExists { .. }),
        "expected TenantRootAlreadyExists, got: {err:?}"
    );
}

/// TC-TRU-03: Non-tenant root may coexist alongside a tenant root (RG is a forest).
#[tokio::test]
async fn non_tenant_root_alongside_tenant_root_allowed() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Tenant root.
    let tenant_type = create_tenant_type(&type_svc).await;
    let tenant_root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(tenant_type.code.clone(), "MainTenant".to_owned()),
            tenant_id,
        )
        .await
        .expect("tenant root");

    // Non-tenant root (auxiliary forest, e.g. "workspace") — created with a
    // regular can_be_root type whose code does NOT start with TENANT_RG_TYPE_PATH.
    let workspace_type = common::create_root_type(&type_svc, "workspace").await;
    let workspace = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(workspace_type.code.clone(), "Workspaces".to_owned()),
            tenant_root.hierarchy.tenant_id,
        )
        .await
        .expect("non-tenant root must be allowed alongside tenant root");
    assert!(workspace.hierarchy.parent_id.is_none());
}

/// TC-TRU-04: `update_group` that would turn a group into a second tenant root
/// (set `parent_id = NULL` while type is tenant-type) is rejected.
#[tokio::test]
async fn tenant_root_update_to_second_root_rejected() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Tenant root #1 (root type — no parents allowed at root level).
    let root_type = create_tenant_type(&type_svc).await;
    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "Root".to_owned()),
            tenant_id,
        )
        .await
        .expect("tenant root");

    // Sub-tenant type: another tenant-type group placed under root_type.
    let sub_type = create_tenant_sub_type(&type_svc, &root_type.code).await;
    let child = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(sub_type.code.clone(), "SubTenant".to_owned())
                .with_parent_id(Some(root.id)),
            root.hierarchy.tenant_id,
        )
        .await
        .expect("sub-tenant under root");

    // Attempt to promote child to a root (parent_id = None) — must deny,
    // the tenant root already exists. For a tenant-type sub-tenant the
    // effective tenant_id equals its own id (derived by code-prefix), so the
    // caller's scope must target that tenant to pass the AuthZ pre-check.
    let child_ctx = common::make_ctx(child.hierarchy.tenant_id);
    let err = group_svc
        .update_group(
            &child_ctx,
            child.id,
            UpdateGroupRequest {
                name: child.name.clone(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .expect_err("promoting sub-tenant to a second root must fail");
    assert!(
        matches!(err, DomainError::TenantRootAlreadyExists { .. }),
        "expected TenantRootAlreadyExists, got: {err:?}"
    );
}

/// TC-TRU-05: Idempotent update of the existing tenant root (no parent change,
/// no type change) does not spuriously trip the uniqueness check.
#[tokio::test]
async fn tenant_root_self_update_allowed() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let tenant_type = create_tenant_type(&type_svc).await;
    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(tenant_type.code.clone(), "RootA".to_owned()),
            tenant_id,
        )
        .await
        .expect("tenant root");

    // Rename only — still tenant-type, still root; existing_root_id == group_id,
    // so the check must NOT raise TenantRootAlreadyExists. Target the root's
    // own tenant scope so the AuthZ pre-check finds it.
    let root_ctx = common::make_ctx(root.hierarchy.tenant_id);
    let updated = group_svc
        .update_group(
            &root_ctx,
            root.id,
            UpdateGroupRequest {
                name: "RootB".to_owned(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .expect("self-update of the only tenant root must succeed");
    assert_eq!(updated.name, "RootB");
}

/// `get_group_unscoped` resolves a group by id with no caller context and no
/// tenant scope — the AuthZ-bypassing read backing the in-process PDP
/// membership contract. Returns the group regardless of which tenant owns it.
#[tokio::test]
async fn get_group_unscoped_returns_group_without_ctx() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "unscopedget").await;
    let group =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "UnscopedGet", tenant_id)
            .await;

    let loaded = group_svc
        .get_group_unscoped(group.id)
        .await
        .expect("get_group_unscoped returns the group");
    assert_eq!(loaded.id, group.id);
    assert_eq!(loaded.hierarchy.tenant_id, tenant_id);
}

/// `get_group_unscoped` surfaces `GroupNotFound` for an absent id.
#[tokio::test]
async fn get_group_unscoped_missing_is_not_found() {
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());

    let err = group_svc
        .get_group_unscoped(Uuid::now_v7())
        .await
        .expect_err("absent group -> NotFound");
    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "expected GroupNotFound, got: {err:?}"
    );
}

/// Creating a group with an id that is already taken yields
/// `GroupAlreadyExists`, not a generic database error. The second create runs
/// in a different tenant, so this pins the group id as globally unique rather
/// than unique per tenant.
#[tokio::test]
async fn group_create_duplicate_id_is_already_exists() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = common::make_ctx(tenant_a);
    let ctx_b = common::make_ctx(tenant_b);
    let root_type = common::create_root_type(&type_svc, "dupid").await;

    let id = Uuid::now_v7();
    let req = |name: &str| {
        CreateGroupRequest::new(root_type.code.clone(), name.to_owned()).with_id(Some(id))
    };

    group_svc
        .create_group(&ctx_a, req("First"), tenant_a)
        .await
        .expect("first create should succeed");

    let err = group_svc
        .create_group(&ctx_b, req("Second"), tenant_b)
        .await
        .expect_err("the id is taken globally, so another tenant cannot reuse it");

    assert!(
        matches!(err, DomainError::GroupAlreadyExists { id: got } if got == id),
        "expected GroupAlreadyExists({id}), got: {err:?}"
    );

    // Verify entity state: the rejected insert left the first group untouched
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.name, "First");
    assert_eq!(model.tenant_id, tenant_a);
}

/// Duplicate id inside the caller's own tenant also yields `GroupAlreadyExists`.
#[tokio::test]
async fn group_create_duplicate_id_same_tenant_is_already_exists() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let root_type = common::create_root_type(&type_svc, "dupidsame").await;

    let id = Uuid::now_v7();
    let req = |name: &str| {
        CreateGroupRequest::new(root_type.code.clone(), name.to_owned()).with_id(Some(id))
    };

    group_svc
        .create_group(&ctx, req("First"), tenant)
        .await
        .expect("first create should succeed");

    let err = group_svc
        .create_group(&ctx, req("Second"), tenant)
        .await
        .expect_err("the id is already taken in this tenant");

    assert!(
        matches!(err, DomainError::GroupAlreadyExists { id: got } if got == id),
        "expected GroupAlreadyExists({id}), got: {err:?}"
    );
}

// =========================================================================
// `$filter` type-path resolution
//
// A `type` filter names a GTS path; the column holds a surrogate SMALLINT.
// `list_groups` therefore walks the filter AST, collects every type literal,
// resolves them in one query, and substitutes the ids back. Each node shape
// is a separate arm of that walk, and the scale test elsewhere in this suite
// only ever builds `type in (...)` -- so the plain comparison, the boolean
// combinations, the negation and the unknown-path rejection went unexercised.
// =========================================================================

/// Build a `list_groups` query from an OData filter string.
fn filter_query(expr: &str) -> ODataQuery {
    let parsed = toolkit_odata::parse_filter_string(expr).expect("filter should parse");
    ODataQuery::new().with_filter(parsed.into_expr())
}

#[tokio::test]
async fn list_groups_filters_by_a_single_type() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t_a = common::create_root_type(&type_svc, "fone").await;
    let t_b = common::create_root_type(&type_svc, "ftwo").await;
    let a = common::create_root_group(&group_svc, &ctx, &t_a.code, "a", tenant_id).await;
    common::create_root_group(&group_svc, &ctx, &t_b.code, "b", tenant_id).await;

    let page = group_svc
        .list_groups(&ctx, &filter_query(&format!("type eq '{}'", t_a.code)))
        .await
        .expect("list_groups with a type filter should succeed");

    let ids: Vec<Uuid> = page.items.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![a.id], "only the group of the filtered type");
    assert_eq!(
        page.items[0].code, t_a.code,
        "the surrogate id must be mapped back to the GTS path on the way out"
    );
}

#[tokio::test]
async fn list_groups_filters_by_either_of_two_types() {
    // `or` is a Composite node: the walk has to recurse into every child to
    // find the literals, and again to substitute them.
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t_a = common::create_root_type(&type_svc, "fora").await;
    let t_b = common::create_root_type(&type_svc, "forb").await;
    let t_c = common::create_root_type(&type_svc, "forc").await;
    let a = common::create_root_group(&group_svc, &ctx, &t_a.code, "a", tenant_id).await;
    let b = common::create_root_group(&group_svc, &ctx, &t_b.code, "b", tenant_id).await;
    common::create_root_group(&group_svc, &ctx, &t_c.code, "c", tenant_id).await;

    let page = group_svc
        .list_groups(
            &ctx,
            &filter_query(&format!("type eq '{}' or type eq '{}'", t_a.code, t_b.code)),
        )
        .await
        .expect("list_groups with an or-filter should succeed");

    let mut ids: Vec<Uuid> = page.items.iter().map(|g| g.id).collect();
    ids.sort();
    let mut want = vec![a.id, b.id];
    want.sort();
    assert_eq!(ids, want, "both branches of the or, and nothing else");
}

#[tokio::test]
async fn list_groups_filters_by_negated_type() {
    // `not` wraps a single child; the walk descends through it in both
    // passes, and a miss there would silently drop the substitution and leave
    // a GTS path where the column wants a SMALLINT.
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t_a = common::create_root_type(&type_svc, "fnota").await;
    let t_b = common::create_root_type(&type_svc, "fnotb").await;
    common::create_root_group(&group_svc, &ctx, &t_a.code, "a", tenant_id).await;
    let b = common::create_root_group(&group_svc, &ctx, &t_b.code, "b", tenant_id).await;

    let page = group_svc
        .list_groups(
            &ctx,
            &filter_query(&format!("not (type eq '{}')", t_a.code)),
        )
        .await
        .expect("list_groups with a negated type filter should succeed");

    let ids: Vec<Uuid> = page.items.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![b.id], "everything except the excluded type");
}

#[tokio::test]
async fn list_groups_rejects_an_unknown_type_in_the_filter() {
    // The resolution map is built from the paths that exist. A path that is
    // absent has no id to substitute, and the request is a client error --
    // not an empty page, which would read as "no such groups" rather than
    // "no such type", and not a 500.
    let db = common::test_db().await;
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let err = group_svc
        .list_groups(
            &ctx,
            &filter_query("type eq 'gts.cf.core.rg.type.v1~x.test.absent.v1~'"),
        )
        .await
        .expect_err("an unresolvable type path must be refused");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected a validation error, got: {err:?}"
    );
    assert!(
        err.to_string().contains("Unknown type in filter"),
        "the message should name the problem: {err}"
    );
}

#[tokio::test]
async fn list_groups_leaves_a_filter_without_a_type_alone() {
    // Nothing to resolve: the walk must pass the node through untouched
    // rather than rewriting or rejecting it.
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let t = common::create_root_type(&type_svc, "fnamed").await;
    let a = common::create_root_group(&group_svc, &ctx, &t.code, "keep-me", tenant_id).await;
    common::create_root_group(&group_svc, &ctx, &t.code, "drop-me", tenant_id).await;

    let page = group_svc
        .list_groups(&ctx, &filter_query("name eq 'keep-me'"))
        .await
        .expect("a non-type filter should pass through untouched");

    let ids: Vec<Uuid> = page.items.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![a.id]);
}

// =========================================================================
// `update_group` isolation-escalation race (uncovered lines 498-519, 1233)
// =========================================================================

/// A `GroupRepositoryTrait` decorator that delegates everything to the real
/// [`GroupRepository`], except that the *first* call to `find_model_by_id`
/// for one watched group also reparents that group in the database, as a
/// side effect, before returning.
///
/// This stages the exact race `update_group`'s doc comment describes:
/// `update_group` reads the group once, before opening its transaction, to
/// guess whether the request changes the parent (cheap, pre-transaction
/// isolation-level hint); this decorator lets a "concurrent" reparent land
/// in the gap between that guess and the authoritative in-transaction
/// re-read, without needing a second thread or a real concurrent client --
/// SQLite's single pooled connection (`max_conns: 1`, see
/// [`common::test_db`]) makes the side effect's write visible to every
/// later read on the same connection, deterministically.
///
/// With `stale_unlocked_parent` set, it stages the *other* half of the same
/// race, the one a single connection cannot produce on its own: the reparent
/// lands on the first in-transaction read, and every **unlocked** model read
/// then answers with the parent the group had *before* it -- what a
/// READ COMMITTED transaction sees from its own snapshot while the writer
/// commits beside it. Only the locked read (`FOR UPDATE`, which on
/// PostgreSQL waits for that writer and re-reads) reports the new parent.
/// A transaction that decides "no parent change" from the unlocked read
/// then writes the old `parent_id` back over the committed move while the
/// closure table keeps the move's ancestry.
struct ParentFlippingGroupRepo {
    inner: GroupRepository,
    watched_group_id: Uuid,
    new_parent_id: Uuid,
    /// When set, unlocked model reads of the watched group report this
    /// parent regardless of what the row now holds -- the stale snapshot
    /// described above. `None` disables the simulation entirely.
    stale_unlocked_parent: Option<Uuid>,
    fired: AtomicBool,
}

impl ParentFlippingGroupRepo {
    fn new(watched_group_id: Uuid, new_parent_id: Uuid) -> Self {
        Self {
            inner: GroupRepository,
            watched_group_id,
            new_parent_id,
            stale_unlocked_parent: None,
            fired: AtomicBool::new(false),
        }
    }

    /// The stale-snapshot variant: the reparent fires on the first
    /// in-transaction read, and unlocked model reads keep answering
    /// `stale_parent_id` afterwards.
    fn with_stale_unlocked_reads(
        watched_group_id: Uuid,
        new_parent_id: Uuid,
        stale_parent_id: Uuid,
    ) -> Self {
        Self {
            inner: GroupRepository,
            watched_group_id,
            new_parent_id,
            stale_unlocked_parent: Some(stale_parent_id),
            fired: AtomicBool::new(false),
        }
    }

    /// Perform the "concurrent" reparent once, on this connection.
    async fn flip_once<C: DBRunner>(&self, db: &C, id: Uuid) -> Result<(), DomainError> {
        if id != self.watched_group_id || self.fired.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(model) = self.inner.find_model_by_id(db, id).await? {
            self.inner
                .update(
                    db,
                    id,
                    Some(self.new_parent_id),
                    model.gts_type_id,
                    &model.name,
                    model.metadata.as_ref(),
                )
                .await?;
            // A real move rewrites the closure table too -- and that is the
            // half a lost update cannot put back. Without it this decorator
            // would stage a race whose two halves cancel out.
            self.inner
                .rebuild_subtree_closure(db, id, Some(self.new_parent_id))
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl GroupRepositoryTrait for ParentFlippingGroupRepo {
    async fn find_by_id<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ResourceGroupSdkModel>, DomainError> {
        // In the stale-snapshot variant the "concurrent" reparent lands
        // here -- on the first read `update_group_inner` makes -- so that
        // every later read in this transaction has to decide for itself
        // whether it can see it.
        if self.stale_unlocked_parent.is_some() {
            self.flip_once(db, id).await?;
        }
        self.inner.find_by_id(db, scope, id).await
    }

    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<RgModel>, DomainError> {
        if let Some(stale_parent) = self.stale_unlocked_parent {
            let mut result = self.inner.find_model_by_id(db, id).await?;
            if id == self.watched_group_id
                && let Some(model) = &mut result
            {
                model.parent_id = Some(stale_parent);
            }
            return Ok(result);
        }
        let result = self.inner.find_model_by_id(db, id).await?;
        if id == self.watched_group_id
            && !self.fired.swap(true, Ordering::SeqCst)
            && let Some(model) = &result
        {
            // The "concurrent" write: reparent the watched group right now,
            // on the same (pre-transaction) connection, using the model just
            // read so name/metadata round-trip unchanged.
            self.inner
                .update(
                    db,
                    id,
                    Some(self.new_parent_id),
                    model.gts_type_id,
                    &model.name,
                    model.metadata.as_ref(),
                )
                .await?;
        }
        Ok(result)
    }

    async fn find_model_by_id_for_update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<RgModel>, DomainError> {
        self.inner.find_model_by_id_for_update(db, id).await
    }

    async fn find_root_id_with_type_prefix<C: DBRunner>(
        &self,
        db: &C,
        type_prefix: &str,
    ) -> Result<Option<Uuid>, DomainError> {
        self.inner
            .find_root_id_with_type_prefix(db, type_prefix)
            .await
    }

    async fn list_groups<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupSdkModel>, DomainError> {
        self.inner.list_groups(db, scope, query).await
    }

    async fn get_descendants<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        self.inner.get_descendants(db, scope, group_id, query).await
    }

    async fn get_ancestors<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        self.inner.get_ancestors(db, scope, group_id, query).await
    }

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<RgModel, DomainError> {
        self.inner
            .insert(db, id, parent_id, gts_type_id, name, metadata, tenant_id)
            .await
    }

    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<u64, DomainError> {
        self.inner
            .update(db, id, parent_id, gts_type_id, name, metadata)
            .await
    }

    async fn delete_by_id<C: DBRunner>(&self, db: &C, id: Uuid) -> Result<(), DomainError> {
        self.inner.delete_by_id(db, id).await
    }

    async fn insert_closure_self_row<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError> {
        self.inner.insert_closure_self_row(db, group_id).await
    }

    async fn insert_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        child_id: Uuid,
        parent_id: Uuid,
    ) -> Result<u64, DomainError> {
        self.inner
            .insert_ancestor_closure_rows(db, child_id, parent_id)
            .await
    }

    async fn delete_by_id_many<C: DBRunner>(
        &self,
        db: &C,
        ids: &[Uuid],
    ) -> Result<(), DomainError> {
        self.inner.delete_by_id_many(db, ids).await
    }

    async fn delete_memberships_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError> {
        self.inner.delete_memberships_many(db, group_ids).await
    }

    async fn delete_all_closure_rows_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError> {
        self.inner.delete_all_closure_rows_many(db, group_ids).await
    }

    async fn get_descendant_ids_with_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<Vec<(Uuid, i32)>, DomainError> {
        self.inner.get_descendant_ids_with_depth(db, group_id).await
    }

    async fn get_depth<C: DBRunner>(&self, db: &C, group_id: Uuid) -> Result<i32, DomainError> {
        self.inner.get_depth(db, group_id).await
    }

    async fn get_max_descendant_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<i32, DomainError> {
        self.inner.get_max_descendant_depth(db, group_id).await
    }

    async fn count_children<C: DBRunner>(
        &self,
        db: &C,
        parent_id: Uuid,
    ) -> Result<u64, DomainError> {
        self.inner.count_children(db, parent_id).await
    }

    async fn is_descendant<C: DBRunner>(
        &self,
        db: &C,
        potential_ancestor: Uuid,
        potential_descendant: Uuid,
    ) -> Result<bool, DomainError> {
        self.inner
            .is_descendant(db, potential_ancestor, potential_descendant)
            .await
    }

    async fn delete_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        keep_self: bool,
    ) -> Result<(), DomainError> {
        self.inner
            .delete_ancestor_closure_rows(db, group_id, keep_self)
            .await
    }

    async fn delete_all_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError> {
        self.inner.delete_all_closure_rows(db, group_id).await
    }

    async fn rebuild_subtree_closure<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
    ) -> Result<u64, DomainError> {
        self.inner
            .rebuild_subtree_closure(db, group_id, new_parent_id)
            .await
    }

    async fn has_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<bool, DomainError> {
        self.inner.has_memberships(db, group_id).await
    }

    async fn resolve_type_paths_batch<C: DBRunner>(
        &self,
        db: &C,
        type_ids: &[i16],
    ) -> Result<std::collections::HashMap<i16, String>, DomainError> {
        self.inner.resolve_type_paths_batch(db, type_ids).await
    }
}

/// Records only `isolation_escalation` calls; every other `RgMetricsPort`
/// method is a no-op. Used to prove `update_group` closed the race by
/// escalating exactly once, not by looping or silently accepting the stale
/// hint.
#[derive(Default)]
struct EscalationRecorder {
    isolation_escalations: AtomicU64,
}

impl RgMetricsPort for EscalationRecorder {
    fn operation_duration(&self, _operation: Operation, _outcome: Outcome, _seconds: f64) {}
    fn subtree_nodes(&self, _operation: Operation, _nodes: u64) {}
    fn closure_rows_written(&self, _operation: Operation, _rows: u64) {}
    fn isolation_escalation(&self, _operation: Operation) {
        self.isolation_escalations.fetch_add(1, Ordering::SeqCst);
    }
    fn metadata_validation_duration(&self, _operation: Operation, _seconds: f64) {}
}

/// The lost-update half of the same race, and the one the pre-transaction
/// hint cannot save the caller from: a rename opens at the backend default,
/// a concurrent reparent commits while it is open, and the rename's own
/// UPDATE assigns `parent_id` unconditionally with a predicate of `id`
/// alone. If the in-transaction read is unlocked, it answers from a
/// snapshot taken before that commit, `parent_changed` is false, and the
/// UPDATE puts the old parent back -- while `rebuild_subtree_closure` has
/// already written the move's ancestry into the closure table. The two
/// disagree from then on, and nothing detects it: an UPDATE by primary key
/// under READ COMMITTED never raises `40001`, and SSI pairs only
/// transactions that are all `SERIALIZABLE`.
///
/// The locked read is what closes it. It waits for the in-flight move and
/// returns the parent that move committed, so the escalation path this
/// file's sibling test covers is reached at all.
#[tokio::test]
async fn group_rename_does_not_revert_a_concurrent_reparent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let bootstrap_group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "lostupd").await;
    let child_type =
        common::create_child_type(&type_svc, "lostupdchild", &[&root_type.code], &[]).await;

    let parent_a = common::create_root_group(
        &bootstrap_group_svc,
        &ctx,
        &root_type.code,
        "ParentA",
        tenant_id,
    )
    .await;
    let parent_b = common::create_root_group(
        &bootstrap_group_svc,
        &ctx,
        &root_type.code,
        "ParentB",
        tenant_id,
    )
    .await;
    let group = common::create_child_group(
        &bootstrap_group_svc,
        &ctx,
        &child_type.code,
        parent_a.id,
        "Original",
        tenant_id,
    )
    .await;

    // Unlocked reads keep reporting `parent_a` after the reparent commits;
    // only the locked read sees `parent_b`.
    let racing_repo = Arc::new(ParentFlippingGroupRepo::with_stale_unlocked_reads(
        group.id,
        parent_b.id,
        parent_a.id,
    ));
    let group_svc = GroupService::new(
        db.clone(),
        QueryProfile::default(),
        common::make_enforcer(),
        racing_repo,
        Arc::new(TypeRepository),
        common::make_types_registry(),
    );

    // A pure rename: the request carries the parent the group already had.
    group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "Renamed".to_owned(),
                parent_id: Some(parent_a.id),
                metadata: None,
            },
        )
        .await
        .expect("rename must succeed");

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(group.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.name, "Renamed");

    // Whichever parent the row ends up with, the closure table must agree
    // with it. A rename that reverted the committed move while the closure
    // kept the move's ancestry is the corruption this guards.
    common::assert_closure_matches_parent_links(&conn).await;
}

/// The race `update_group`'s module doc and the `UpdateGroupOutcome` doc
/// describe: the pre-transaction hint says "no parent change" (a pure
/// rename), so the transaction opens below `SERIALIZABLE`; a concurrent
/// write reparents the group in the gap before the authoritative
/// in-transaction read runs. That read must see the new parent, refuse to
/// proceed below `SERIALIZABLE` (`NeedsSerializable`, without writing
/// anything), and the caller must escalate and retry -- ending with the
/// group back under its original parent (the request never asked to move
/// it) and the new name applied.
///
/// Exercises the currently-uncovered `update_group` escalation branch
/// (lines ~498-519) and `UpdateGroupOutcome::NeedsSerializable` (line
/// ~1233), and -- via [`EscalationRecorder`] wired through `with_metrics`
/// (line ~189) -- confirms the close is observable as exactly one
/// `isolation_escalation(Operation::Update)` recording.
#[tokio::test]
async fn group_update_escalates_to_serializable_on_concurrent_reparent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let bootstrap_group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "escroot").await;
    let child_type =
        common::create_child_type(&type_svc, "escchild", &[&root_type.code], &[]).await;

    let parent_a = common::create_root_group(
        &bootstrap_group_svc,
        &ctx,
        &root_type.code,
        "ParentA",
        tenant_id,
    )
    .await;
    let parent_b = common::create_root_group(
        &bootstrap_group_svc,
        &ctx,
        &root_type.code,
        "ParentB",
        tenant_id,
    )
    .await;
    let group = common::create_child_group(
        &bootstrap_group_svc,
        &ctx,
        &child_type.code,
        parent_a.id,
        "Original",
        tenant_id,
    )
    .await;

    let recorder = Arc::new(EscalationRecorder::default());
    let flipping_repo = Arc::new(ParentFlippingGroupRepo::new(group.id, parent_b.id));
    let group_svc = GroupService::new(
        db.clone(),
        QueryProfile::default(),
        common::make_enforcer(),
        flipping_repo,
        Arc::new(TypeRepository),
        common::make_types_registry(),
    )
    .with_metrics(recorder.clone());

    // The request itself asks only to rename, keeping the parent it already
    // has (`parent_a`) -- the pre-transaction hint will therefore say "no
    // move", which is exactly the case the decorator's side effect races.
    let updated = group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                name: "Renamed".to_owned(),
                parent_id: Some(parent_a.id),
                metadata: None,
            },
        )
        .await
        .expect("update must succeed after escalating to SERIALIZABLE");

    assert_eq!(updated.name, "Renamed");
    assert_eq!(
        updated.hierarchy.parent_id,
        Some(parent_a.id),
        "the group must end up back under its original parent, not the \
         concurrent write's target"
    );
    assert_eq!(
        recorder.isolation_escalations.load(Ordering::SeqCst),
        1,
        "the race must be resolved by exactly one escalation to SERIALIZABLE"
    );

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let model = RgEntity::find()
        .filter(RgColumn::Id.eq(group.id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("found");
    assert_eq!(model.parent_id, Some(parent_a.id));
    assert_eq!(model.name, "Renamed");

    common::assert_closure_matches_parent_links(&conn).await;
}
