// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-rest-api:p1
#![allow(clippy::expect_used)]
//! Full-chain integration test with a real (`SQLite` in-memory) database.
//!
//! Verifies the complete `AuthZ` -> `PolicyEnforcer` -> `GroupService` -> `AccessScope`
//! -> `SecureORM` -> SQL WHERE `tenant_id` IN (...) -> filtered results path.
//!
//! Two tenants each create groups; listing groups through the `AuthZ`-scoped
//! `GroupService` returns only the requesting tenant's data.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, PolicyEnforcer,
    constraints::{Constraint, InGroupPredicate, InPredicate, Predicate},
};
use modkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use modkit_odata::ODataQuery;
use modkit_security::{SecurityContext, pep_properties};
use sea_orm_migration::MigratorTrait;

use cf_resource_group::domain::group_service::{GroupService, QueryProfile};
use cf_resource_group::domain::type_service::TypeService;
use cf_resource_group::infra::storage::group_repo::GroupRepository;
use cf_resource_group::infra::storage::membership_repo::MembershipRepository;
use cf_resource_group::infra::storage::migrations::Migrator;
use cf_resource_group::infra::storage::type_repo::TypeRepository;

// ── Mock AuthZ: tenant-scoping (like static-authz-plugin) ───────────────

struct TenantScopingAuthZ;

#[async_trait]
impl AuthZResolverClient for TenantScopingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("subject must have tenant_id");

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant_id],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

async fn test_db() -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect to in-memory SQLite");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    Arc::new(DBProvider::new(db))
}

fn make_group_service(
    db: Arc<DBProvider<DbError>>,
) -> GroupService<GroupRepository, TypeRepository> {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(TenantScopingAuthZ);
    let enforcer = PolicyEnforcer::new(authz);
    GroupService::new(
        db,
        QueryProfile::default(),
        enforcer,
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        common::make_types_registry(),
    )
}

// ── Tests ───────────────────────────────────────────────────────────────

/// Full chain: two tenants create groups, each tenant sees only its own.
///
/// Flow per tenant:
///   `SecurityContext{tenant=T}` -> `GroupService.list_groups(&ctx, &query)`
///     -> `PolicyEnforcer.access_scope()` -> `AccessScope{owner_tenant_id IN (T)}`
///     -> `GroupRepository.list_groups(&conn, &scope, &query)`
///       -> `SecureORM` `.scope_with(&scope)` -> SQL WHERE `tenant_id` IN ('T')
///     -> only T's groups returned
// Scenario: L2-Tenant-01 - Tenant isolation for list_groups
#[tokio::test]
async fn tenant_isolation_list_groups() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    // Create a type (types are not tenant-scoped)
    let type_code = format!(
        "gts.cf.core.rg.type.v1~x.test.dbiso{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create type");

    // Tenant A creates 2 groups
    let ga1 = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code.clone(),
                name: "Tenant A - Group 1".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create group A1");

    let ga2 = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code.clone(),
                name: "Tenant A - Group 2".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create group A2");

    // Tenant B creates 1 group
    let gb1 = group_svc
        .create_group(
            &ctx_b,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code.clone(),
                name: "Tenant B - Group 1".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_b,
        )
        .await
        .expect("create group B1");

    let query = ODataQuery::default();

    // ── Tenant A lists groups: should see only A's groups ──
    let page_a = group_svc
        .list_groups(&ctx_a, &query)
        .await
        .expect("list groups for tenant A");

    let ids_a: Vec<Uuid> = page_a.items.iter().map(|g| g.id).collect();
    assert!(ids_a.contains(&ga1.id), "Tenant A should see group A1");
    assert!(ids_a.contains(&ga2.id), "Tenant A should see group A2");
    assert!(!ids_a.contains(&gb1.id), "Tenant A must NOT see group B1");
    assert_eq!(
        ids_a.len(),
        2,
        "Tenant A should see exactly 2 groups, got: {ids_a:?}"
    );

    // ── Tenant B lists groups: should see only B's groups ──
    let page_b = group_svc
        .list_groups(&ctx_b, &query)
        .await
        .expect("list groups for tenant B");

    let ids_b: Vec<Uuid> = page_b.items.iter().map(|g| g.id).collect();
    assert!(ids_b.contains(&gb1.id), "Tenant B should see group B1");
    assert!(!ids_b.contains(&ga1.id), "Tenant B must NOT see group A1");
    assert!(!ids_b.contains(&ga2.id), "Tenant B must NOT see group A2");
    assert_eq!(
        ids_b.len(),
        1,
        "Tenant B should see exactly 1 group, got: {ids_b:?}"
    );
}

/// Full chain: `get_group` with wrong tenant returns not-found.
// Scenario: L2-Tenant-02 - Cross-tenant get_group invisible
#[tokio::test]
async fn tenant_isolation_get_group_cross_tenant_invisible() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    // Create type
    let type_code = format!(
        "gts.cf.core.rg.type.v1~x.test.xget{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create type");

    // Tenant A creates a group
    let ga = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code,
                name: "A's secret group".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create group for tenant A");

    // Tenant B tries to get tenant A's group → should fail
    let result = group_svc.get_group(&ctx_b, ga.id).await;
    assert!(
        result.is_err(),
        "Tenant B should not be able to get tenant A's group"
    );
}

/// Full chain: `get_group_descendants` respects tenant scope.
// Scenario: L2-Tenant-03 - Hierarchy queries are tenant-scoped
#[tokio::test]
async fn tenant_isolation_hierarchy_scoped() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    // Create parent and child types
    let parent_type = format!(
        "gts.cf.core.rg.type.v1~x.test.hierp{}.v1~",
        Uuid::now_v7().as_simple()
    );
    let child_type = format!(
        "gts.cf.core.rg.type.v1~x.test.hierc{}.v1~",
        Uuid::now_v7().as_simple()
    );

    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: parent_type.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create parent type");

    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: child_type.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent_type.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create child type");

    // Tenant A: parent + child
    let parent = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: parent_type.clone(),
                name: "A Parent".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create parent");

    let _child = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: child_type.clone(),
                name: "A Child".to_owned(),
                parent_id: Some(parent.id),
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create child");

    // Tenant B: unrelated group (same parent type, different tenant)
    let _b_group = group_svc
        .create_group(
            &ctx_b,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: parent_type,
                name: "B Unrelated".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_b,
        )
        .await
        .expect("create B group");

    // Tenant A lists hierarchy from parent — should NOT include B's group
    let query = ODataQuery::default();
    let hier = group_svc
        .get_group_descendants(&ctx_a, parent.id, &query)
        .await
        .expect("list hierarchy for tenant A");

    let hier_names: Vec<&str> = hier.items.iter().map(|g| g.name.as_str()).collect();
    assert!(
        hier_names.contains(&"A Parent"),
        "hierarchy should contain parent"
    );
    assert!(
        hier_names.contains(&"A Child"),
        "hierarchy should contain child"
    );
    assert!(
        !hier_names.iter().any(|n| n.contains("B Unrelated")),
        "hierarchy must NOT contain tenant B's group, got: {hier_names:?}"
    );
}

/// Full chain: `update_group` with wrong tenant returns not-found.
// Scenario: L2-Tenant-04 - Cross-tenant update blocked
#[tokio::test]
async fn tenant_isolation_update_cross_tenant_blocked() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    let type_code = format!(
        "gts.cf.core.rg.type.v1~x.test.xupd{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create type");

    // Tenant A creates a group
    let ga = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code.clone(),
                name: "A's group".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create group for tenant A");

    // Tenant B tries to update tenant A's group → should fail
    let result = group_svc
        .update_group(
            &ctx_b,
            ga.id,
            resource_group_sdk::UpdateGroupRequest {
                type_path: type_code,
                name: "Hijacked!".to_owned(),
                parent_id: None,
                metadata: None,
            },
        )
        .await;
    assert!(
        result.is_err(),
        "Tenant B should not be able to update tenant A's group"
    );
}

/// Full chain: `delete_group` with wrong tenant returns not-found.
// Scenario: L2-Tenant-05 - Cross-tenant delete blocked
#[tokio::test]
async fn tenant_isolation_delete_cross_tenant_blocked() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx_a = make_ctx(tenant_a);
    let ctx_b = make_ctx(tenant_b);

    let type_code = format!(
        "gts.cf.core.rg.type.v1~x.test.xdel{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: type_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create type");

    // Tenant A creates a group
    let ga = group_svc
        .create_group(
            &ctx_a,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: type_code,
                name: "A's group to delete".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant_a,
        )
        .await
        .expect("create group for tenant A");

    // Tenant B tries to delete tenant A's group → should fail
    let result = group_svc.delete_group(&ctx_b, ga.id, false).await;
    assert!(
        result.is_err(),
        "Tenant B should not be able to delete tenant A's group"
    );

    // Tenant A can still see and delete their own group
    let own = group_svc.get_group(&ctx_a, ga.id).await;
    assert!(own.is_ok(), "Tenant A should still see their group");

    let del = group_svc.delete_group(&ctx_a, ga.id, false).await;
    assert!(
        del.is_ok(),
        "Tenant A should be able to delete their own group"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 2: Group-based predicate tests (InGroup / InGroupSubtree)
// ═══════════════════════════════════════════════════════════════════════

/// Mock `AuthZ` that returns tenant scoping + `InGroup` predicate.
/// Simulates S14 scenario: user has access to specific group IDs.
struct GroupScopingAuthZ {
    /// Groups the subject has access to (injected per test).
    allowed_group_ids: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverClient for GroupScopingAuthZ {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("subject must have tenant_id");

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![
                        // Tenant scoping (always present)
                        Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [tenant_id],
                        )),
                        // Group membership scoping
                        Predicate::InGroup(InGroupPredicate::new(
                            pep_properties::RESOURCE_ID,
                            self.allowed_group_ids.clone(),
                        )),
                    ],
                }],
                deny_reason: None,
            },
        })
    }
}

/// Phase 2 test: `InGroup` predicate compiles to correct `AccessScope`
/// containing both tenant filter AND group membership filter.
// Scenario: L2-Tenant-06 - InGroup predicate produces combined scope (S14)
#[tokio::test]
async fn group_based_in_group_predicate_produces_combined_scope() {
    let group_a = Uuid::now_v7();
    let group_b = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();

    let authz: Arc<dyn AuthZResolverClient> = Arc::new(GroupScopingAuthZ {
        allowed_group_ids: vec![group_a, group_b],
    });
    let enforcer = PolicyEnforcer::new(authz);
    let ctx = make_ctx(tenant_id);

    let scope = enforcer
        .access_scope(
            &ctx,
            &cf_resource_group::domain::group_service::RG_GROUP_RESOURCE,
            "list",
            None,
        )
        .await
        .expect("should succeed");

    // Scope has 1 constraint with 2 filters: In(tenant) AND InGroup(groups)
    assert_eq!(scope.constraints().len(), 1);
    assert_eq!(scope.constraints()[0].filters().len(), 2);

    // First filter: tenant
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_id));

    // Second filter: InGroup
    let group_filter = &scope.constraints()[0].filters()[1];
    assert!(
        matches!(group_filter, modkit_security::ScopeFilter::InGroup(_)),
        "expected InGroup filter, got: {group_filter:?}"
    );
}

/// Phase 2 test: memberships seeded into DB, verify that group-based
/// membership data is correctly stored and accessible.
///
/// This verifies the data layer works with the membership table that
/// `InGroup` subqueries reference. The actual subquery SQL execution
/// is validated by the `SecureORM` cond.rs unit tests.
// Scenario: L2-Membership-01 - Membership data correctly stored
#[tokio::test]
async fn group_based_membership_data_correctly_stored() {
    let db = test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = make_group_service(db.clone());

    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);

    // Create types: project (root, allows "task" members) and task
    let project_type = format!(
        "gts.cf.core.rg.type.v1~x.test.proj{}.v1~",
        Uuid::now_v7().as_simple()
    );
    let task_type = format!(
        "gts.cf.core.rg.type.v1~x.test.task{}.v1~",
        Uuid::now_v7().as_simple()
    );

    // Create task type first (project references it in allowed_membership_types)
    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: task_type.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create task type");

    type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: project_type.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![task_type.clone()],
            metadata_schema: None,
            ..Default::default()
        })
        .await
        .expect("create project type");

    // Create ProjectA and ProjectB
    let project_a = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: project_type.clone(),
                name: "ProjectA".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant,
        )
        .await
        .expect("create ProjectA");

    let project_b = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest {
                id: None,
                type_path: project_type,
                name: "ProjectB".to_owned(),
                parent_id: None,
                metadata: None,
            },
            tenant,
        )
        .await
        .expect("create ProjectB");

    // Add memberships via MembershipService (with PolicyEnforcer)
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(TenantScopingAuthZ);
    let enforcer = PolicyEnforcer::new(authz);
    let membership_svc = cf_resource_group::domain::membership_service::MembershipService::new(
        db.clone(),
        enforcer,
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        Arc::new(MembershipRepository),
    );

    // task-001, task-002 → ProjectA
    membership_svc
        .add_membership(&ctx, project_a.id, &task_type, "task-001")
        .await
        .expect("add task-001 to ProjectA");
    membership_svc
        .add_membership(&ctx, project_a.id, &task_type, "task-002")
        .await
        .expect("add task-002 to ProjectA");

    // task-003 → ProjectB
    membership_svc
        .add_membership(&ctx, project_b.id, &task_type, "task-003")
        .await
        .expect("add task-003 to ProjectB");

    // List memberships for ProjectA
    let query = ODataQuery::default();
    let all = membership_svc
        .list_memberships(&ctx, &query)
        .await
        .expect("list memberships");

    let project_a_members: Vec<&str> = all
        .items
        .iter()
        .filter(|m| m.group_id == project_a.id)
        .map(|m| m.resource_id.as_str())
        .collect();

    assert!(project_a_members.contains(&"task-001"));
    assert!(project_a_members.contains(&"task-002"));
    assert!(!project_a_members.contains(&"task-003"));

    let members_of_b: Vec<&str> = all
        .items
        .iter()
        .filter(|m| m.group_id == project_b.id)
        .map(|m| m.resource_id.as_str())
        .collect();

    assert!(members_of_b.contains(&"task-003"));
    assert!(!members_of_b.contains(&"task-001"));
}
