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

use serde_json::json;
use uuid::Uuid;

use cf_resource_group::domain::error::DomainError;
use cf_resource_group::domain::group_service::{GroupService, QueryProfile};
use cf_resource_group::domain::type_service::TypeService;
use cf_resource_group::infra::storage::entity::resource_group::{
    Column as RgColumn, Entity as RgEntity,
};
use cf_resource_group::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use cf_resource_group::infra::storage::group_repo::GroupRepository;
use cf_resource_group::infra::storage::type_repo::TypeRepository;
use modkit_db::secure::{SecureEntityExt, secure_insert};
use modkit_odata::ODataQuery;
use modkit_security::AccessScope;
use resource_group_sdk::{CreateGroupRequest, CreateTypeRequest, UpdateGroupRequest};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

/// Build a `GroupService` with custom `QueryProfile`.
fn make_group_service_with_profile(
    db: std::sync::Arc<modkit_db::DBProvider<modkit_db::DbError>>,
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: unrelated_type.code.clone(),
                name: "Bad".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "Rootless".to_owned(),
                parent_id: None,
                metadata: None,
            },
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
            CreateGroupRequest {
                id: None,
                type_path: "gts.cf.core.rg.type.v1~x.test.nonexistent.v1~".to_owned(),
                name: "Ghost".to_owned(),
                parent_id: None,
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "CrossTenant".to_owned(),
                parent_id: Some(root_a.id),
                metadata: None,
            },
            tenant_b,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref message } if message.contains("must match parent tenant_id")),
        "Expected Validation with tenant mismatch, got: {err:?}"
    );
}

/// TC-GRP-24: Create group with metadata JSONB.
#[tokio::test]
async fn group_create_with_metadata() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"department": "engineering", "code": 42});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "WithMeta".to_owned(),
                parent_id: None,
                metadata: Some(meta.clone()),
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let root1 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root1", tenant_id).await;
    let root2 =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root2", tenant_id).await;

    assert_ne!(root1.id, root2.id);
    assert_eq!(root1.type_path, root2.type_path);

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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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

/// TC-GRP-06: Move under descendant -> CycleDetected.
#[tokio::test]
async fn group_move_under_descendant_cycle() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create a type that can be both root and child
    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_code = format!(
        "gts.cf.core.rg.type.v1~x.test.flexible{}.v1~",
        Uuid::now_v7().as_simple()
    );
    let _flexible_type = type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![root_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let profile = QueryProfile {
        max_depth: None,
        max_width: Some(1),
    };
    let group_svc = make_group_service_with_profile(db.clone(), profile);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_code = format!(
        "gts.cf.core.rg.type.v1~x.test.flex{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![root_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
                type_path: root_type.code.clone(),
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
    assert_eq!(updated.type_path, root_type.code);

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

/// TC-GRP-10: Update group type -- parent compatibility.
/// InvalidParentType("does not allow current parent type").
#[tokio::test]
async fn group_update_type_parent_incompatible() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;
    // new_type allows only some other type as parent, NOT root_type
    let other_root_type = common::create_root_type(&type_svc, "other").await;
    let incompatible_code = format!(
        "gts.cf.core.rg.type.v1~x.test.incompat{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: incompatible_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![other_root_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create incompatible type");

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

    // Update child to incompatible type
    let err = group_svc
        .update_group(
            &ctx,
            child.id,
            UpdateGroupRequest {
                type_path: incompatible_code,
                name: child.name.clone(),
                parent_id: Some(root.id),
                metadata: None,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { ref message } if message.contains("does not allow")),
        "Expected InvalidParentType, got: {err:?}"
    );
}

/// TC-GRP-11: Update group type -- children compatibility.
/// InvalidParentType("child group... does not allow... as parent type").
#[tokio::test]
async fn group_update_type_children_incompatible() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let mid_type = common::create_child_type(&type_svc, "mid", &[&root_type.code], &[]).await;
    // leaf only allows mid_type as parent
    let leaf_type = common::create_child_type(&type_svc, "leaf", &[&mid_type.code], &[]).await;

    // another_root_type that can be root, but leaf_type does NOT list it as parent
    let another_root_type = common::create_root_type(&type_svc, "alt").await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;
    let mid =
        common::create_child_group(&group_svc, &ctx, &mid_type.code, root.id, "Mid", tenant_id)
            .await;
    let _leaf =
        common::create_child_group(&group_svc, &ctx, &leaf_type.code, mid.id, "Leaf", tenant_id)
            .await;

    // Try to change mid's type to another_root_type. Leaf does not allow another_root_type as parent.
    let err = group_svc
        .update_group(
            &ctx,
            mid.id,
            UpdateGroupRequest {
                type_path: another_root_type.code.clone(),
                name: mid.name.clone(),
                parent_id: Some(root.id),
                metadata: None,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { ref message } if message.contains("does not allow")),
        "Expected InvalidParentType about child group, got: {err:?}"
    );
}

/// TC-GRP-26: Simultaneous type + parent change.
#[tokio::test]
async fn group_update_simultaneous_type_and_parent() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let type_a = common::create_root_type(&type_svc, "orgA").await;
    let type_b = common::create_root_type(&type_svc, "orgB").await;
    // child_type allows both type_a and type_b as parents
    let child_code = format!(
        "gts.cf.core.rg.type.v1~x.test.multichild{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![type_a.code.clone(), type_b.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create multi-child type");

    // new_child_type also allows both
    let new_child_code = format!(
        "gts.cf.core.rg.type.v1~x.test.newchild{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: new_child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![type_a.code.clone(), type_b.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create new child type");

    let root_a =
        common::create_root_group(&group_svc, &ctx, &type_a.code, "RootA", tenant_id).await;
    let root_b =
        common::create_root_group(&group_svc, &ctx, &type_b.code, "RootB", tenant_id).await;
    let child =
        common::create_child_group(&group_svc, &ctx, &child_code, root_a.id, "Child", tenant_id)
            .await;

    // Simultaneous type + parent change
    let updated = group_svc
        .update_group(
            &ctx,
            child.id,
            UpdateGroupRequest {
                type_path: new_child_code.clone(),
                name: "UpdatedChild".to_owned(),
                parent_id: Some(root_b.id),
                metadata: None,
            },
        )
        .await
        .expect("simultaneous update");

    assert_eq!(updated.type_path, new_child_code);
    assert_eq!(updated.hierarchy.parent_id, Some(root_b.id));
    assert_eq!(updated.name, "UpdatedChild");

    // Verify closure after move
    let conn = db.conn().expect("conn");
    common::assert_closure_rows(&conn, child.id, &[(child.id, 0), (root_b.id, 1)]).await;
}

/// TC-GRP-27: Root group -> non-root type change.
#[tokio::test]
async fn group_update_root_to_nonroot_type() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let nonroot_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Try to change root's type to a non-root type while keeping parent_id=None
    let err = group_svc
        .update_group(
            &ctx,
            root.id,
            UpdateGroupRequest {
                type_path: nonroot_type.code.clone(),
                name: root.name.clone(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::InvalidParentType { ref message } if message.contains("cannot be a root group")),
        "Expected InvalidParentType with 'cannot be a root group', got: {err:?}"
    );
}

/// TC-GRP-28: Update with nonexistent new type_path.
#[tokio::test]
async fn group_update_nonexistent_type() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    let err = group_svc
        .update_group(
            &ctx,
            root.id,
            UpdateGroupRequest {
                type_path: "gts.cf.core.rg.type.v1~x.test.ghost.v1~".to_owned(),
                name: "Root".to_owned(),
                parent_id: None,
                metadata: None,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::TypeNotFound { .. }),
        "Expected TypeNotFound, got: {err:?}"
    );
}

// =========================================================================
// Group delete tests (TC-GRP-12, 13, 14, 15, 34, 35)
// =========================================================================

/// TC-GRP-12: Delete leaf group.
/// Success, no group, closure rows removed.
#[tokio::test]
async fn group_delete_leaf() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let root =
        common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_id).await;

    // Insert membership directly
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let membership = membership_entity::ActiveModel {
        group_id: Set(root.id),
        gts_type_id: Set(1),
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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

    // Add a membership to child (direct insert)
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let membership = membership_entity::ActiveModel {
        group_id: Set(child.id),
        gts_type_id: Set(1),
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "TooDeep".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "Child2".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
        "gts.cf.core.rg.type.v1~x.test.sub{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: sub_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![child_type.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
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
        "gts.cf.core.rg.type.v1~x.test.subsub{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(CreateTypeRequest {
            code: subsub_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![sub_code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: grandchild_type.code.clone(),
                name: "Grandchild".to_owned(),
                parent_id: Some(child.id),
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "Child3".to_owned(),
                parent_id: Some(root.id),
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: String::new(),
                parent_id: None,
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let long_name = "x".repeat(256);
    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: long_name,
                parent_id: None,
                metadata: None,
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"self_managed": true});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "BarrierGroup".to_owned(),
                parent_id: None,
                metadata: Some(meta.clone()),
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "RichMeta".to_owned(),
                parent_id: None,
                metadata: Some(meta.clone()),
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let old_meta = json!({"old_key": "old_value", "shared": 1});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "ReplaceMe".to_owned(),
                parent_id: None,
                metadata: Some(old_meta),
            },
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
                type_path: root_type.code.clone(),
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "NoMeta".to_owned(),
                parent_id: None,
                metadata: None,
            },
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
                type_path: root_type.code.clone(),
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;

    let meta = json!({"initial": true});
    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "WithMeta".to_owned(),
                parent_id: None,
                metadata: Some(meta),
            },
            tenant_id,
        )
        .await
        .expect("create group");

    let updated = group_svc
        .update_group(
            &ctx,
            group.id,
            UpdateGroupRequest {
                type_path: root_type.code.clone(),
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
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
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "BarrierChild".to_owned(),
                parent_id: Some(root.id),
                metadata: Some(json!({"self_managed": true})),
            },
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
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let root_type = common::create_root_type(&type_svc, "org").await;
    let child_type = common::create_child_type(&type_svc, "dept", &[&root_type.code], &[]).await;

    let root_meta = json!({"level": "root"});
    let root = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "Root".to_owned(),
                parent_id: None,
                metadata: Some(root_meta.clone()),
            },
            tenant_id,
        )
        .await
        .expect("create root");

    let child_meta = json!({"level": "child", "barrier": false});
    let child = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: child_type.code.clone(),
                name: "Child".to_owned(),
                parent_id: Some(root.id),
                metadata: Some(child_meta.clone()),
            },
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
    type_svc: &cf_resource_group::domain::type_service::TypeService<TypeRepository>,
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
    let tenant_code = format!("gts.cf.core.rg.type.v1~x.test.{suffix_t}.v1~");

    // Tenant type: create first without self-reference, then update
    type_svc
        .create_type(CreateTypeRequest {
            code: tenant_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![user_type.code.clone()],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create tenant type");

    let tenant_type = type_svc
        .update_type(
            &tenant_code,
            resource_group_sdk::UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![tenant_code.clone()],
                allowed_membership_types: vec![user_type.code.clone()],
                metadata_schema: None,
                ..Default::default()
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (_, dept_type, _, _, _) = create_adr_types(&type_svc).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: dept_type.code.clone(),
                name: "RootDept".to_owned(),
                parent_id: None,
                metadata: None,
            },
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let (tenant_type, _, branch_type, _, _) = create_adr_types(&type_svc).await;

    let t1 = common::create_root_group(&group_svc, &ctx, &tenant_type.code, "T1", tenant_id).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest {
                id: None,
                type_path: branch_type.code.clone(),
                name: "BadBranch".to_owned(),
                parent_id: Some(t1.id),
                metadata: None,
            },
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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

    let msg = err.to_string();
    assert!(
        msg.contains("not in allowed_membership_types")
            || msg.contains("allowed_membership_types")
            || msg.contains("Validation"),
        "Expected membership validation error, got: {msg}"
    );
}

/// TC-ADR-07: Same user in multiple groups (D8 + T7).
#[tokio::test]
async fn adr_same_user_in_multiple_groups() {
    let db = common::test_db().await;
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "SQLMetaGroup".to_owned(),
                parent_id: None,
                metadata: Some(evil_meta.clone()),
            },
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
    let type_svc = cf_resource_group::domain::type_service::TypeService::new(
        db.clone(),
        Arc::new(TypeRepository),
    );
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
            CreateGroupRequest {
                id: None,
                type_path: root_type.code.clone(),
                name: "BigMetaGroup".to_owned(),
                parent_id: None,
                metadata: Some(big_meta.clone()),
            },
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
        Err(e) => {
            // Acceptable to reject large payloads
            let msg = e.to_string();
            assert!(
                !msg.contains("panic"),
                "Should not panic on large metadata: {msg}"
            );
        }
    }
}
