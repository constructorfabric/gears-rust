// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-membership:p1
#![allow(clippy::expect_used, clippy::doc_markdown)]
//! Membership service integration tests (Phase 4).
//!
//! Tests the MembershipService domain logic: add/remove lifecycle,
//! allowed_membership_types validation, tenant compatibility, and duplicate detection.

mod common;

use common::{
    create_root_type, create_root_type_with_memberships, make_ctx, make_group_service,
    make_membership_service, test_db,
};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use resource_group::domain::error::DomainError;
use resource_group::infra::storage::entity::resource_group_membership::{
    Column as MbrColumn, Entity as MbrEntity,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::SecureEntityExt;
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;

// TC-MBR-01: Add membership happy path
#[tokio::test]
async fn membership_add_happy_path() {
    let db = test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create a member resource type first (must be registered)
    let member_type = types.new_member_type("mbr");
    // Create a group type that allows membership of the member type
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);

    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-001")
        .await
        .expect("add membership should succeed");

    assert_eq!(result.group_id, group.id);
    assert_eq!(result.resource_type, member_type);
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
    let types = common::TestTypes::new();
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");

    let err = mbr_svc
        .add_membership(&ctx, Uuid::now_v7(), &member_type, "res-001")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-dup")
        .await
        .expect("first add should succeed");

    let err = mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-dup")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create a group type that allows some registered type (we just need a valid group)
    let registered_type = types.new_member_type("reg");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&registered_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // Try to add membership with a type path that is NOT registered in the Types Registry
    let err = mbr_svc
        .add_membership(
            &ctx,
            group.id,
            gts_id!("cf.fake.nonexistent.type.v1~"),
            "res-001",
        )
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create two types: one allowed, one not
    let allowed_type = types.new_member_type("allowed");
    let disallowed_type = types.new_member_type("disallowed");

    // Group only allows `allowed_type`
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&allowed_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .add_membership(&ctx, group.id, &disallowed_type, "res-001")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);

    let group_a =
        common::create_root_group(&group_svc, &ctx_a, &grp_type.code, "GA", tenant_a).await;
    let group_b =
        common::create_root_group(&group_svc, &ctx_b, &grp_type.code, "GB", tenant_b).await;

    // Add resource to tenant A group
    mbr_svc
        .add_membership(&ctx_a, group_a.id, &member_type, "shared-res")
        .await
        .expect("add to tenant A should succeed");

    // Try to add same resource to tenant B group
    let err = mbr_svc
        .add_membership(&ctx_b, group_b.id, &member_type, "shared-res")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-rm")
        .await
        .expect("add membership");

    mbr_svc
        .remove_membership(&ctx, group.id, &member_type, "res-rm")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .remove_membership(&ctx, group.id, &member_type, "nonexistent")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let type_a = types.new_member_type("typea");
    let type_b = types.new_member_type("typeb");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&type_a, &type_b]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    mbr_svc
        .add_membership(&ctx, group.id, &type_a, "res-a")
        .await
        .expect("add type A membership");
    mbr_svc
        .add_membership(&ctx, group.id, &type_b, "res-b")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // First membership for a new resource always succeeds (no tenant conflict possible)
    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type, "brand-new-res")
        .await;
    assert!(result.is_ok(), "first membership should always succeed");
}

// TC-MBR-11: Empty resource_id
#[tokio::test]
async fn membership_empty_resource_id() {
    let db = test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    // Document behavior: empty resource_id is accepted (no domain validation on it)
    let result = mbr_svc
        .add_membership(&ctx, group.id, &member_type, "")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .remove_membership(
            &ctx,
            group.id,
            gts_id!("cf.fake.unregistered.type.v1~"),
            "res-001",
        )
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    // Group type with NO allowed memberships
    let grp_type = create_root_type(&types, "grp_empty");
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let err = mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-001")
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
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("mbr");
    let grp_type = create_root_type_with_memberships(&types, "grp", &[&member_type]);

    let group1 = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;
    let group2 = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G2", tenant).await;

    mbr_svc
        .add_membership(&ctx, group1.id, &member_type, "shared-res")
        .await
        .expect("add to group1");
    mbr_svc
        .add_membership(&ctx, group2.id, &member_type, "shared-res")
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
    let types = common::TestTypes::new();
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let query = ODataQuery::default();
    let page = mbr_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list memberships");

    assert!(page.items.is_empty(), "should be empty with no memberships");
}

/// `list_memberships_unscoped` returns membership rows with no caller context
/// and no tenant scope — the AuthZ-bypassing read backing the in-process PDP
/// membership contract. The caller supplies the OData filter (the PDP uses
/// `resource_id eq <subject>`); here we assert the unfiltered listing returns
/// the seeded membership.
#[tokio::test]
async fn list_memberships_unscoped_returns_rows_without_ctx() {
    let db = test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("umbr");
    let grp_type = create_root_type_with_memberships(&types, "ugrp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "UG1", tenant).await;
    mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-u")
        .await
        .expect("add membership");

    let query = ODataQuery::default();
    let page = mbr_svc
        .list_memberships_unscoped(&query)
        .await
        .expect("list_memberships_unscoped returns rows");

    let count = page.items.iter().filter(|m| m.group_id == group.id).count();
    assert_eq!(count, 1, "seeded membership must be listed");
}

/// `add_membership` reads the resource type and the group type in ONE
/// catalog request.
#[tokio::test]
async fn membership_add_makes_one_catalog_request() {
    let db = test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);
    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("one");
    let grp_type = create_root_type_with_memberships(&types, "onegrp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;

    let before = types.batch_get_calls();
    mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-1")
        .await
        .expect("add membership");
    assert_eq!(types.batch_get_calls() - before, 1);
}

/// A `resource_type` filter that covers the page needs no second catalog
/// request after the query.
#[tokio::test]
async fn list_memberships_with_resource_type_filter_makes_one_catalog_request() {
    let db = test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service(db.clone(), &types);
    let mbr_svc = make_membership_service(db.clone(), &types);
    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    let member_type = types.new_member_type("flt");
    let grp_type = create_root_type_with_memberships(&types, "fltgrp", &[&member_type]);
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "G1", tenant).await;
    mbr_svc
        .add_membership(&ctx, group.id, &member_type, "res-1")
        .await
        .expect("add membership");

    let parsed = toolkit_odata::parse_filter_string(&format!("resource_type eq '{member_type}'"))
        .expect("parse filter");
    let query = ODataQuery::new().with_filter(parsed.into_expr());

    let before = types.batch_get_calls();
    let page = mbr_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list memberships");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].resource_type, member_type);
    assert_eq!(types.batch_get_calls() - before, 1);
}
