// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-membership:p1
#![allow(clippy::expect_used, clippy::doc_markdown)]
//! Membership service integration tests (Phase 4).
//!
//! Tests the MembershipService domain logic: add/remove lifecycle,
//! allowed_membership_types validation, tenant compatibility, and duplicate detection.

mod common;

use std::sync::Arc;

use common::{create_root_type, make_ctx, make_group_service, make_membership_service, test_db};
use modkit_odata::ODataQuery;
use uuid::Uuid;

use cf_resource_group::domain::error::DomainError;
use cf_resource_group::domain::type_service::TypeService;
use cf_resource_group::infra::storage::entity::resource_group_membership::{
    Column as MbrColumn, Entity as MbrEntity,
};
use cf_resource_group::infra::storage::type_repo::TypeRepository;
use modkit_db::secure::SecureEntityExt;
use modkit_security::AccessScope;
use resource_group_sdk::CreateTypeRequest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// Helper: create a root type that allows the given membership type paths.
async fn create_type_with_memberships(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
    memberships: &[&str],
) -> resource_group_sdk::ResourceGroupType {
    let code = format!(
        "gts.cf.core.rg.type.v1~x.test.{}{}.v1~",
        suffix,
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

// TC-MBR-01: Add membership happy path
#[tokio::test]
async fn membership_add_happy_path() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create a member resource type first (must be registered)
    let member_type = create_root_type(&type_svc, "mbr").await;
    // Create a group type that allows membership of the member type
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;

    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-001")
        .await
        .expect("add membership should succeed");

    assert_eq!(result.group_id, group.id);
    assert_eq!(result.resource_type, member_type.code);
    assert_eq!(result.resource_id, "res-001");

    // Direct DB assertion: row exists with correct composite key
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let rows = MbrEntity::find()
        .filter(MbrColumn::GroupId.eq(group.id))
        .filter(MbrColumn::ResourceId.eq("res-001"))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query membership table");
    assert_eq!(rows.len(), 1, "expected exactly 1 membership row");
    assert_eq!(rows[0].group_id, group.id);
    assert_eq!(rows[0].resource_id, "res-001");
}

// TC-MBR-02: Add to nonexistent group
#[tokio::test]
async fn membership_add_nonexistent_group() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;

    let err = mbr_svc
        .add_membership(&ctx, Uuid::now_v7(), &member_type.code, "res-001")
        .await
        .expect_err("should fail for nonexistent group");

    assert!(
        matches!(err, DomainError::GroupNotFound { .. }),
        "expected GroupNotFound, got: {err:?}"
    );
}

// TC-MBR-03: Add duplicate membership
#[tokio::test]
async fn membership_add_duplicate() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-dup")
        .await
        .expect("first add should succeed");

    let err = mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-dup")
        .await
        .expect_err("duplicate add should fail");

    assert!(
        matches!(
            err,
            DomainError::Conflict { .. }
                | DomainError::ConflictActiveReferences { .. }
                | DomainError::DuplicateMembership { .. }
        ),
        "expected Conflict, ConflictActiveReferences, or DuplicateMembership, got: {err:?}"
    );
}

// TC-MBR-04: Unregistered resource_type
#[tokio::test]
async fn membership_add_unregistered_resource_type() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create a group type that allows some registered type (we just need a valid group)
    let registered_type = create_root_type(&type_svc, "reg").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&registered_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // Try to add membership with a type path that is NOT registered in gts_type table
    let err = mbr_svc
        .add_membership(&ctx, group.id, "gts.x.fake.nonexistent.v1~", "res-001")
        .await
        .expect_err("unregistered type should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
    assert!(
        format!("{err:?}").contains("Unknown resource type"),
        "error should mention unknown resource type: {err:?}"
    );
}

// TC-MBR-05: resource_type not in allowed_membership_types
#[tokio::test]
async fn membership_add_not_in_allowed_membership_types() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create two types: one allowed, one not
    let allowed_type = create_root_type(&type_svc, "allowed").await;
    let disallowed_type = create_root_type(&type_svc, "disallowed").await;

    // Group only allows `allowed_type`
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&allowed_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .add_membership(&ctx, group.id, &disallowed_type.code, "res-001")
        .await
        .expect_err("disallowed type should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
    assert!(
        format!("{err:?}").contains("not in allowed_membership_types"),
        "error should mention allowed_membership_types: {err:?}"
    );
}

// TC-MBR-06: Tenant compatibility violation
#[tokio::test]
async fn membership_add_tenant_incompatibility() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;

    let group_a =
        common::create_root_group(&group_svc, &ctx_a, &grp_type.code, "GA", tenant_a).await;
    let group_b =
        common::create_root_group(&group_svc, &ctx_b, &grp_type.code, "GB", tenant_b).await;

    // Add resource to tenant A group
    mbr_svc
        .add_membership(&ctx_a, group_a.id, &member_type.code, "shared-res")
        .await
        .expect("add to tenant A should succeed");

    // Try to add same resource to tenant B group
    let err = mbr_svc
        .add_membership(&ctx_b, group_b.id, &member_type.code, "shared-res")
        .await
        .expect_err("cross-tenant should fail");

    assert!(
        matches!(err, DomainError::TenantIncompatibility { .. }),
        "expected TenantIncompatibility, got: {err:?}"
    );
}

// TC-MBR-07: Remove existing membership
#[tokio::test]
async fn membership_remove_existing() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-rm")
        .await
        .expect("add membership");

    mbr_svc
        .remove_membership(&ctx, group.id, &member_type.code, "res-rm")
        .await
        .expect("remove should succeed");

    // Direct DB assertion: row gone
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let rows = MbrEntity::find()
        .filter(MbrColumn::GroupId.eq(group.id))
        .filter(MbrColumn::ResourceId.eq("res-rm"))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query membership table");
    assert!(
        rows.is_empty(),
        "membership row should be gone after remove"
    );
}

// TC-MBR-08: Remove nonexistent membership
#[tokio::test]
async fn membership_remove_nonexistent() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .remove_membership(&ctx, group.id, &member_type.code, "nonexistent")
        .await
        .expect_err("remove nonexistent should fail");

    assert!(
        matches!(err, DomainError::MembershipNotFound { .. }),
        "expected MembershipNotFound, got: {err:?}"
    );
}

// TC-MBR-09: Multiple resource types in same group
#[tokio::test]
async fn membership_multiple_resource_types_same_group() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let type_a = create_root_type(&type_svc, "typeA").await;
    let type_b = create_root_type(&type_svc, "typeB").await;
    let grp_type =
        create_type_with_memberships(&type_svc, "grp", &[&type_a.code, &type_b.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &type_a.code, "res-a")
        .await
        .expect("add type A membership");
    mbr_svc
        .add_membership(&ctx, group.id, &type_b.code, "res-b")
        .await
        .expect("add type B membership");

    let query = ODataQuery::default();
    let page = mbr_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list memberships");

    let group_member_count = page.items.iter().filter(|m| m.group_id == group.id).count();
    assert_eq!(group_member_count, 2, "should have 2 memberships");
}

// TC-MBR-10: First membership always allowed (tenant)
#[tokio::test]
async fn membership_first_always_allowed_tenant() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // First membership for a new resource always succeeds (no tenant conflict possible)
    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "brand-new-res")
        .await;
    assert!(result.is_ok(), "first membership should always succeed");
}

// TC-MBR-11: Empty resource_id
#[tokio::test]
async fn membership_empty_resource_id() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // Document behavior: empty resource_id is accepted (no domain validation on it)
    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "")
        .await;
    // The service does not validate resource_id content, so this should succeed
    assert!(
        result.is_ok(),
        "empty resource_id is accepted by the domain: {result:?}"
    );
}

// TC-MBR-12: Remove with unregistered resource_type
#[tokio::test]
async fn membership_remove_unregistered_resource_type() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .remove_membership(&ctx, group.id, "gts.x.fake.unregistered.v1~", "res-001")
        .await
        .expect_err("remove with unregistered type should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
    assert!(
        format!("{err:?}").contains("Unknown resource type"),
        "error should mention unknown resource type: {err:?}"
    );
}

// TC-MBR-13: Empty allowed_membership_types rejects all
#[tokio::test]
async fn membership_empty_allowed_membership_types_rejects_all() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    // Group type with NO allowed memberships
    let grp_type = create_root_type(&type_svc, "grp_empty").await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-001")
        .await
        .expect_err("empty allowed_membership_types should reject");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
    assert!(
        format!("{err:?}").contains("not in allowed_membership_types"),
        "error should mention allowed_membership_types: {err:?}"
    );
}

// TC-MBR-14: Same resource in multiple groups same tenant
#[tokio::test]
async fn membership_same_resource_multiple_groups_same_tenant() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = create_root_type(&type_svc, "mbr").await;
    let grp_type = create_type_with_memberships(&type_svc, "grp", &[&member_type.code]).await;

    let group1 = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;
    let group2 = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G2", tenant).await;

    mbr_svc
        .add_membership(&ctx, group1.id, &member_type.code, "shared-res")
        .await
        .expect("add to group1");
    mbr_svc
        .add_membership(&ctx, group2.id, &member_type.code, "shared-res")
        .await
        .expect("add to group2 same tenant should succeed");

    // Direct DB assertion: two rows with different group_id for same resource
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let rows = MbrEntity::find()
        .filter(MbrColumn::ResourceId.eq("shared-res"))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query membership table");
    assert_eq!(
        rows.len(),
        2,
        "expected 2 membership rows for same resource"
    );
    let group_ids: Vec<Uuid> = rows.iter().map(|r| r.group_id).collect();
    assert!(group_ids.contains(&group1.id));
    assert!(group_ids.contains(&group2.id));
}

// TC-MBR-15: List memberships empty result
#[tokio::test]
async fn membership_list_empty() {
    let db = test_db().await;
    let mbr_svc = make_membership_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let query = ODataQuery::default();
    let page = mbr_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list memberships");

    assert!(page.items.is_empty(), "should be empty with no memberships");
}
