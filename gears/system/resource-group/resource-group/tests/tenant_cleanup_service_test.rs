#![allow(clippy::expect_used, clippy::doc_markdown)]
//! `RgTenantCleanupService` integration tests.
//!
//! `ResourceGroupTenantCleanup::delete_tenant_groups` removes a hard-deleted
//! tenant's groups of one type without `AuthZ` enforcement. Every test drives
//! the service over a **deny-all** enforcer: a scoped path would be rejected,
//! so success proves the cleanup never consulted AuthZ. Fixtures are seeded
//! through allow-all services against the same DB.

mod common;

use std::sync::Arc;

use uuid::Uuid;

use resource_group::domain::tenant_cleanup_service::RgTenantCleanupService;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group_sdk::ResourceGroupTenantCleanup;

use common::{CreateTypeRequest, ResourceGroupType, TestTypes};

/// Root type (`can_be_root = true`) that accepts the given membership types.
async fn root_type_with_memberships(
    types: &TestTypes,
    suffix: &str,
    memberships: &[&str],
) -> ResourceGroupType {
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        toolkit_gts::gts_id!("cf.core.rg.group.v1~"),
        suffix,
        Uuid::now_v7().as_simple()
    );
    types
        .create_type_unscoped(CreateTypeRequest {
            code,
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: memberships.iter().map(|s| (*s).to_owned()).collect(),
            metadata_schema: None,
        })
        .await
        .expect("create root type with memberships")
}

/// Cleanup service whose underlying `GroupService` denies every scoped call.
fn cleanup_service(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
) -> RgTenantCleanupService<GroupRepository> {
    RgTenantCleanupService::new(Arc::new(common::make_group_service_deny(db)))
}

/// True when a group with this id still exists (read unscoped).
async fn exists(db: &Arc<toolkit_db::DBProvider<toolkit_db::DbError>>, id: Uuid) -> bool {
    common::make_group_service(db.clone())
        .get_group_unscoped(id)
        .await
        .is_ok()
}

/// Only the named type in the named tenant goes, with each root's subtree and
/// memberships; other types and other tenants are untouched, and a second run
/// is a no-op.
#[tokio::test]
async fn deletes_only_the_tenants_groups_of_the_type_and_is_idempotent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let mbr_svc = common::make_membership_service(db.clone());

    let member_type = common::create_root_type(&type_svc, "tcmbr").await;
    let target_type = root_type_with_memberships(&type_svc, "tctarget", &[&member_type.code]).await;
    let child_type =
        common::create_child_type(&type_svc, "tcchild", &[&target_type.code], &[]).await;
    let other_type = common::create_root_type(&type_svc, "tcother").await;

    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let root = common::create_root_group(&group_svc, &ctx, &target_type.code, "Root", tenant).await;
    let child =
        common::create_child_group(&group_svc, &ctx, &child_type.code, root.id, "Child", tenant)
            .await;
    mbr_svc
        .add_membership(&ctx, root.id, &member_type.code, "res-tc")
        .await
        .expect("add membership");
    let other =
        common::create_root_group(&group_svc, &ctx, &other_type.code, "Other", tenant).await;

    let foreign_tenant = Uuid::now_v7();
    let foreign_ctx = common::make_ctx(foreign_tenant);
    let foreign = common::create_root_group(
        &group_svc,
        &foreign_ctx,
        &target_type.code,
        "Foreign",
        foreign_tenant,
    )
    .await;

    let cleanup = cleanup_service(db.clone());
    let removed = cleanup
        .delete_tenant_groups(&common::make_anon_ctx(), tenant, &target_type.code)
        .await
        .expect("cleanup succeeds despite the deny-all enforcer");

    // The root and the child in its subtree.
    assert_eq!(removed, 2);
    assert!(!exists(&db, root.id).await, "target root removed");
    assert!(
        !exists(&db, child.id).await,
        "subtree removed with its root"
    );
    assert!(exists(&db, other.id).await, "another type is untouched");
    assert!(exists(&db, foreign.id).await, "another tenant is untouched");

    let again = cleanup
        .delete_tenant_groups(&common::make_anon_ctx(), tenant, &target_type.code)
        .await
        .expect("second cleanup succeeds");
    assert_eq!(again, 0, "nothing left to remove");
}

/// A group of the named type whose parent has another type is a root of the
/// matching set: it is removed, and its parent stays.
#[tokio::test]
async fn removes_groups_nested_under_a_foreign_type_parent() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());

    let parent_type = common::create_root_type(&type_svc, "tcparent").await;
    let child_type =
        common::create_child_type(&type_svc, "tcnested", &[&parent_type.code], &[]).await;

    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let parent =
        common::create_root_group(&group_svc, &ctx, &parent_type.code, "Parent", tenant).await;
    let first = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        parent.id,
        "First",
        tenant,
    )
    .await;
    let second = common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        parent.id,
        "Second",
        tenant,
    )
    .await;

    let removed = cleanup_service(db.clone())
        .delete_tenant_groups(&common::make_anon_ctx(), tenant, &child_type.code)
        .await
        .expect("cleanup succeeds");

    assert_eq!(removed, 2);
    assert!(!exists(&db, first.id).await);
    assert!(!exists(&db, second.id).await);
    assert!(
        exists(&db, parent.id).await,
        "the foreign-type parent stays"
    );
}

/// A tenant that owns no group of the type is a successful no-op.
#[tokio::test]
async fn tenant_without_groups_of_the_type_is_a_no_op() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let target_type = common::create_root_type(&type_svc, "tcempty").await;

    let removed = cleanup_service(db)
        .delete_tenant_groups(&common::make_anon_ctx(), Uuid::now_v7(), &target_type.code)
        .await
        .expect("cleanup succeeds");
    assert_eq!(removed, 0);
}

/// A type code no stored group has ever used selects nothing: the cleanup
/// matches through RG's local type dictionary, which only knows types groups
/// were created with, so there is nothing of that type to remove.
#[tokio::test]
async fn type_without_stored_groups_is_a_no_op() {
    let db = common::test_db().await;
    let unused = format!(
        "{}x.test.tcunused.i{}.v1~",
        toolkit_gts::gts_id!("cf.core.rg.group.v1~"),
        Uuid::now_v7().as_simple()
    );

    let removed = cleanup_service(db)
        .delete_tenant_groups(&common::make_anon_ctx(), Uuid::now_v7(), &unused)
        .await
        .expect("cleanup succeeds");
    assert_eq!(removed, 0);
}
