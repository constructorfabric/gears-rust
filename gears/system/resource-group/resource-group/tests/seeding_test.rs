// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-seeding:p1
#![allow(clippy::expect_used, clippy::doc_markdown)]
//! Seeding integration tests (Phase 4).
//!
//! Tests idempotent seed_types, seed_groups, seed_memberships functions.
//!
//! The seeding functions internally use `SecurityContext::anonymous()` which maps
//! to nil tenant in the AllowAll mock. Therefore, seeding tests create groups
//! with nil tenant to ensure visibility through anonymous-scoped queries.

mod common;

use std::sync::Arc;

use common::{make_ctx, make_group_service, make_membership_service, test_db};
use uuid::Uuid;

use resource_group::domain::seeding::{
    GroupSeedDef, MembershipSeedDef, seed_groups, seed_memberships, seed_types,
};
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::gts_type::{
    Column as TypeColumn, Entity as TypeEntity,
};
use resource_group::infra::storage::entity::resource_group::Entity as GroupEntity;
use resource_group::infra::storage::entity::resource_group_membership::{
    Column as MbrColumn, Entity as MbrEntity,
};
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::CreateTypeRequest;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::AccessScope;

/// Nil tenant matches `SecurityContext::anonymous()` used internally by seeding.
fn nil_tenant() -> Uuid {
    Uuid::nil()
}

fn nil_ctx() -> toolkit_security::SecurityContext {
    common::make_anon_ctx()
}

fn unique_type_code(suffix: &str) -> String {
    // 5-token GTS segment per ADR-001 Finding 2:
    // vendor.package.namespace.type.vMAJOR. Suffix goes in namespace
    // (lowercased); UUID-hex (with `i` prefix to start with a letter)
    // goes in type.
    format!(
        "gts.cf.core.rg.type.v1~x.test.{}.i{}.v1~",
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    )
}

// TC-SEED-01: seed_types creates missing type
#[tokio::test]
async fn seed_types_creates_missing() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = unique_type_code("seed");
    let seeds = vec![CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    }];

    let result = seed_types(&type_svc, &seeds).await.expect("seed_types");
    assert_eq!(result.created, 1);
    assert_eq!(result.unchanged, 0);
    assert_eq!(result.updated, 0);

    // DB assertion: type exists in gts_type table
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let rows = TypeEntity::find()
        .filter(TypeColumn::SchemaId.eq(&code))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query type table");
    assert_eq!(rows.len(), 1, "type should exist in DB");
}

// TC-SEED-02: seed_types skips unchanged type
#[tokio::test]
async fn seed_types_skips_unchanged() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = unique_type_code("seed");
    let seeds = vec![CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    }];

    seed_types(&type_svc, &seeds)
        .await
        .expect("seed_types run 1");

    let result = seed_types(&type_svc, &seeds)
        .await
        .expect("seed_types run 2");
    assert_eq!(result.created, 0);
    assert_eq!(result.unchanged, 1);
    assert_eq!(result.updated, 0);
}

// TC-SEED-03: seed_types updates changed type
#[tokio::test]
async fn seed_types_updates_changed() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // Create a membership type first so we can reference it
    let mbr_code = unique_type_code("mbr");
    type_svc
        .create_type(CreateTypeRequest {
            code: mbr_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create membership type");

    let code = unique_type_code("seed");
    let seeds_v1 = vec![CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    }];

    seed_types(&type_svc, &seeds_v1)
        .await
        .expect("seed_types v1");

    // Change allowed_membership_types
    let seeds_v2 = vec![CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![mbr_code.clone()],
        metadata_schema: None,
    }];

    let result = seed_types(&type_svc, &seeds_v2)
        .await
        .expect("seed_types v2");
    assert_eq!(result.updated, 1);
    assert_eq!(result.created, 0);

    let updated = type_svc.get_type(&code).await.expect("get_type");
    assert_eq!(
        updated.allowed_membership_types,
        vec![mbr_code],
        "allowed_membership_types should be updated"
    );
}

// TC-SEED-04: seed_types idempotent (3 runs)
#[tokio::test]
async fn seed_types_idempotent_three_runs() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = unique_type_code("seed");
    let seeds = vec![CreateTypeRequest {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    }];

    let r1 = seed_types(&type_svc, &seeds).await.expect("run 1");
    assert_eq!(r1.created, 1);

    let r2 = seed_types(&type_svc, &seeds).await.expect("run 2");
    assert_eq!(r2.unchanged, 1);
    assert_eq!(r2.created, 0);

    let r3 = seed_types(&type_svc, &seeds).await.expect("run 3");
    assert_eq!(r3.unchanged, 1);
    assert_eq!(r3.created, 0);
}

// TC-SEED-05: seed_groups creates groups with closure
#[tokio::test]
async fn seed_groups_creates_with_closure() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant = nil_tenant();

    let type_code = unique_type_code("sroot");
    type_svc
        .create_type(CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    let id1 = Uuid::now_v7();
    let id2 = Uuid::now_v7();

    let seeds = vec![
        GroupSeedDef {
            id: id1,
            code: type_code.clone(),
            name: "Seed Root 1".to_owned(),
            parent_id: None,
            metadata: None,
            tenant_id: tenant,
        },
        GroupSeedDef {
            id: id2,
            code: type_code.clone(),
            name: "Seed Root 2".to_owned(),
            parent_id: None,
            metadata: None,
            tenant_id: tenant,
        },
    ];

    let result = seed_groups(&group_svc, &seeds).await.expect("seed_groups");
    assert_eq!(result.created, 2);

    // Verify groups exist in DB
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let groups = GroupEntity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query groups");
    assert!(groups.len() >= 2, "at least 2 groups should exist");

    // Verify closure: each root group has a self-referencing closure row
    let group_ids: Vec<Uuid> = groups.iter().map(|g| g.id).collect();
    common::assert_closure_count(&conn, &group_ids, group_ids.len()).await;
}

// TC-SEED-06: seed_groups skips existing
#[tokio::test]
async fn seed_groups_skips_existing() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant = nil_tenant();
    let ctx = nil_ctx();

    let type_code = unique_type_code("sgrp");
    type_svc
        .create_type(CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // Create group with nil tenant so seed_groups (using anonymous ctx) can see it
    let group =
        common::create_root_group(&group_svc, &ctx, &type_code, "Pre-existing", tenant).await;

    // Seed with the same ID -- should be found and skipped
    let seeds = vec![GroupSeedDef {
        id: group.id,
        code: type_code.clone(),
        name: "Pre-existing".to_owned(),
        parent_id: None,
        metadata: None,
        tenant_id: tenant,
    }];

    let result = seed_groups(&group_svc, &seeds).await.expect("seed_groups");
    assert_eq!(result.unchanged, 1);
    assert_eq!(result.created, 0);
}

// TC-SEED-07: seed_memberships creates links
#[tokio::test]
async fn seed_memberships_creates_links() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = nil_tenant();
    let ctx = nil_ctx();

    let member_type = common::create_root_type(&type_svc, "mbr").await;
    let grp_type_code = unique_type_code("sgrp");
    type_svc
        .create_type(CreateTypeRequest {
            code: grp_type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let group = common::create_root_group(&group_svc, &ctx, &grp_type_code, "G1", tenant).await;

    let seeds = vec![
        MembershipSeedDef {
            group_id: group.id,
            resource_type: member_type.code.clone(),
            resource_id: "seed-res-1".to_owned(),
        },
        MembershipSeedDef {
            group_id: group.id,
            resource_type: member_type.code.clone(),
            resource_id: "seed-res-2".to_owned(),
        },
    ];

    let result = seed_memberships(&mbr_svc, &seeds)
        .await
        .expect("seed_memberships");
    assert_eq!(result.created, 2);

    // DB assertion: membership rows exist
    let conn = db.conn().expect("db conn");
    let scope = AccessScope::allow_all();
    let rows = MbrEntity::find()
        .filter(MbrColumn::GroupId.eq(group.id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query membership table");
    assert_eq!(rows.len(), 2);
}

// TC-SEED-08: seed_memberships handles duplicates
#[tokio::test]
async fn seed_memberships_handles_duplicates() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    let tenant = nil_tenant();
    let ctx = nil_ctx();

    let member_type = common::create_root_type(&type_svc, "mbr").await;
    let grp_type_code = unique_type_code("sgrp");
    type_svc
        .create_type(CreateTypeRequest {
            code: grp_type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let group = common::create_root_group(&group_svc, &ctx, &grp_type_code, "G1", tenant).await;

    let seeds = vec![MembershipSeedDef {
        group_id: group.id,
        resource_type: member_type.code.clone(),
        resource_id: "seed-dup".to_owned(),
    }];

    let r1 = seed_memberships(&mbr_svc, &seeds)
        .await
        .expect("seed run 1");
    assert_eq!(r1.created, 1);

    // Second run: duplicate. The seed contract is idempotent — DuplicateMembership
    // from add_membership is caught and counted as `unchanged`, so the call must
    // succeed.
    let r2 = seed_memberships(&mbr_svc, &seeds)
        .await
        .expect("duplicate seed should be idempotent");
    assert_eq!(r2.unchanged, 1, "duplicate should be counted as unchanged");
    assert_eq!(r2.created, 0);
}

// TC-SEED-09: seed_memberships skips tenant-incompatible
#[tokio::test]
async fn seed_memberships_skips_tenant_incompatible() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());
    let mbr_svc = make_membership_service(db.clone());

    // Use two different real tenants for the groups
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    let member_type = common::create_root_type(&type_svc, "mbr").await;
    let grp_type_code = unique_type_code("sgrp");
    type_svc
        .create_type(CreateTypeRequest {
            code: grp_type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let group_a =
        common::create_root_group(&group_svc, &ctx_a, &grp_type_code, "GA", tenant_a).await;
    let group_b =
        common::create_root_group(&group_svc, &ctx_b, &grp_type_code, "GB", tenant_b).await;

    // Add resource to tenant A directly
    mbr_svc
        .add_membership(&ctx_a, group_a.id, &member_type.code, "cross-res")
        .await
        .expect("add to tenant A");

    // Seed tries to add same resource to tenant B group -- should be skipped
    let seeds = vec![MembershipSeedDef {
        group_id: group_b.id,
        resource_type: member_type.code.clone(),
        resource_id: "cross-res".to_owned(),
    }];

    let result = seed_memberships(&mbr_svc, &seeds)
        .await
        .expect("seed_memberships");
    assert_eq!(result.skipped, 1, "tenant-incompatible should be skipped");
    assert_eq!(result.created, 0);
}

// TC-SEED-10: seed_types with empty list
#[tokio::test]
async fn seed_types_empty_list() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let result = seed_types(&type_svc, &[]).await.expect("seed_types empty");
    assert_eq!(result.created, 0);
    assert_eq!(result.updated, 0);
    assert_eq!(result.unchanged, 0);
    assert_eq!(result.skipped, 0);
}

// TC-SEED-11: seed_groups wrong order (child before parent)
#[tokio::test]
async fn seed_groups_wrong_order_child_before_parent() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant = nil_tenant();

    let parent_code = unique_type_code("sparent");
    let child_code = unique_type_code("schild");

    type_svc
        .create_type(CreateTypeRequest {
            code: parent_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create parent type");

    type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent_code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create child type");

    let parent_id = Uuid::now_v7();
    let child_id = Uuid::now_v7();

    // Wrong order: child before parent
    let seeds = vec![
        GroupSeedDef {
            id: child_id,
            code: child_code.clone(),
            name: "Child First".to_owned(),
            parent_id: Some(parent_id),
            metadata: None,
            tenant_id: tenant,
        },
        GroupSeedDef {
            id: parent_id,
            code: parent_code.clone(),
            name: "Parent Second".to_owned(),
            parent_id: None,
            metadata: None,
            tenant_id: tenant,
        },
    ];

    let result = seed_groups(&group_svc, &seeds).await;
    assert!(result.is_err(), "child before parent should error");
}

// TC-SEED-12: seed_memberships with nonexistent group
#[tokio::test]
async fn seed_memberships_nonexistent_group() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let mbr_svc = make_membership_service(db.clone());

    let member_type = common::create_root_type(&type_svc, "mbr").await;

    let seeds = vec![MembershipSeedDef {
        group_id: Uuid::now_v7(),
        resource_type: member_type.code.clone(),
        resource_id: "orphan-res".to_owned(),
    }];

    let result = seed_memberships(&mbr_svc, &seeds).await;
    assert!(result.is_err(), "nonexistent group should error");
}
