// @cpt-dod:cpt-cf-resource-group-dod-testing-integration-auth:p1
#![allow(clippy::expect_used, clippy::doc_markdown)]
//! `RgReadService` integration tests for the membership reads.
//!
//! `get_group` and `list_memberships` on `ResourceGroupReadHierarchy` are the
//! AuthZ-bypassing reads an in-process PDP uses to resolve a subject's groups
//! while *being* the PDP (it cannot re-enter the `PolicyEnforcer`).
//!
//! The bypass is proven by wiring the read service with a **deny-all** enforcer
//! and asserting the reads still succeed: a scoped path would be rejected by
//! that enforcer, so success means the read never consulted AuthZ. Fixtures are
//! seeded through allow-all services against the same DB.

mod common;

use std::sync::Arc;

use toolkit_odata::ODataQuery;
use uuid::Uuid;

use resource_group::domain::read_service::RgReadService;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::{CreateTypeRequest, ResourceGroupReadHierarchy, ResourceGroupType};

/// Root type (`can_be_root = true`) that accepts the given membership types.
async fn root_type_with_memberships(
    type_svc: &TypeService<TypeRepository>,
    suffix: &str,
    memberships: &[&str],
) -> ResourceGroupType {
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
        .expect("create root type with memberships")
}

/// `ResourceGroupReadHierarchy::get_group` resolves even when the underlying
/// service is wired with a deny-all enforcer — proving it bypasses AuthZ.
#[tokio::test]
async fn read_service_get_group_bypasses_authz() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);

    // Seed through allow-all services.
    let group_svc = common::make_group_service(db.clone());
    let root_type = common::create_root_type(&type_svc, "rsget").await;
    let group = common::create_root_group(&group_svc, &ctx, &root_type.code, "RSGet", tenant).await;

    // Read through a service wired with a DENY-all enforcer: success proves the
    // read never went through the PolicyEnforcer.
    let read = RgReadService::new(
        Arc::new(common::make_group_service_deny(db.clone())),
        Arc::new(common::make_membership_service_deny(db.clone())),
    );

    let loaded = read
        .get_group(&common::make_anon_ctx(), group.id)
        .await
        .expect("get_group resolves despite the deny-all enforcer");
    assert_eq!(loaded.id, group.id);
    assert_eq!(loaded.hierarchy.tenant_id, tenant);
}

/// `ResourceGroupReadHierarchy::list_memberships` resolves even when the
/// underlying service is wired with a deny-all enforcer — proving the bypass.
#[tokio::test]
async fn read_service_list_memberships_bypasses_authz() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);

    // Seed through allow-all services.
    let group_svc = common::make_group_service(db.clone());
    let mbr_svc = common::make_membership_service(db.clone());
    let member_type = common::create_root_type(&type_svc, "rsmbr").await;
    let grp_type = root_type_with_memberships(&type_svc, "rsgrp", &[&member_type.code]).await;
    let group = common::create_root_group(&group_svc, &ctx, &grp_type.code, "RSGrp", tenant).await;
    mbr_svc
        .add_membership(&ctx, group.id, &member_type.code, "res-rs")
        .await
        .expect("add membership");

    // Read through a DENY-all-backed service: success proves the bypass.
    let read = RgReadService::new(
        Arc::new(common::make_group_service_deny(db.clone())),
        Arc::new(common::make_membership_service_deny(db.clone())),
    );

    let page = read
        .list_memberships(&common::make_anon_ctx(), &ODataQuery::default())
        .await
        .expect("list_memberships resolves despite the deny-all enforcer");
    let count = page.items.iter().filter(|m| m.group_id == group.id).count();
    assert_eq!(count, 1, "seeded membership must be listed");
}
