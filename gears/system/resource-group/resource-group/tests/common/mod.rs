// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
#![allow(dead_code, clippy::expect_used, clippy::doc_markdown)]
//! Shared test helpers for resource-group integration tests.
//!
//! Provides database setup, service construction, security context helpers,
//! and assertion utilities. Used by Phases 2-5 of the unit test plan.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::DenyReason,
};
use sea_orm_migration::MigratorTrait;
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::{SecurityContext, pep_properties};

use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::membership_service::MembershipService;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup as ResourceGroupModel, ResourceGroupType,
};

// -- Stub TypesRegistryClient (returns NotFound for all types — metadata validation skipped) --

struct StubTypesRegistry;

#[async_trait]
impl types_registry_sdk::TypesRegistryClient for StubTypesRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(vec![])
    }

    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(vec![])
    }

    async fn get_type_schema(
        &self,
        type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        Err(types_registry_sdk::testing::not_found(type_id))
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        Err(types_registry_sdk::testing::not_found(
            type_uuid.to_string(),
        ))
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        type_ids
            .into_iter()
            .map(|id| {
                let err = types_registry_sdk::testing::not_found(&id);
                (id, Err(err))
            })
            .collect()
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        type_uuids
            .into_iter()
            .map(|uuid| {
                let err = types_registry_sdk::testing::not_found(uuid.to_string());
                (uuid, Err(err))
            })
            .collect()
    }

    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(vec![])
    }

    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(vec![])
    }

    async fn get_instance(
        &self,
        id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        Err(types_registry_sdk::testing::not_found(id))
    }

    async fn get_instance_by_uuid(
        &self,
        uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        Err(types_registry_sdk::testing::not_found(uuid.to_string()))
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        ids.into_iter()
            .map(|id| {
                let err = types_registry_sdk::testing::not_found(&id);
                (id, Err(err))
            })
            .collect()
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        uuids
            .into_iter()
            .map(|uuid| {
                let err = types_registry_sdk::testing::not_found(uuid.to_string());
                (uuid, Err(err))
            })
            .collect()
    }

    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(vec![])
    }
}

/// Build stub `TypesRegistryClient` for tests.
pub fn make_types_registry() -> Arc<dyn types_registry_sdk::TypesRegistryClient> {
    Arc::new(StubTypesRegistry)
}

// -- AllowAll AuthZ mock --

struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverClient for AllowAllAuthZ {
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
            .unwrap_or(Uuid::nil());

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

// -- DenyAll AuthZ mock --

/// Enforcer that denies every request. Used to prove the unscoped reads bypass
/// the `PolicyEnforcer`: a service wired with this enforcer would reject any
/// scoped operation, so an unscoped read that still succeeds proves it never
/// consulted AuthZ.
struct DenyAllAuthZ;

#[async_trait]
impl AuthZResolverClient for DenyAllAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: vec![],
                deny_reason: Some(DenyReason {
                    error_code: "deny_all".to_owned(),
                    details: Some("deny-all test enforcer".to_owned()),
                }),
            },
        })
    }
}

/// Build a `SecurityContext` for anonymous (nil tenant) -- matches `SecurityContext::anonymous()`.
pub fn make_anon_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

// -- Database setup --

/// Create an in-memory SQLite database with migrations applied.
pub async fn test_db() -> Arc<DBProvider<DbError>> {
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

// -- Security context --

/// Build a `SecurityContext` for the given tenant.
pub fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

/// Build an allow-all `PolicyEnforcer` with tenant scoping.
pub fn make_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowAllAuthZ);
    PolicyEnforcer::new(authz)
}

// -- Type helpers --

/// Create a root type (can_be_root = true, no parents, no memberships).
pub async fn create_root_type(
    svc: &TypeService<TypeRepository>,
    suffix: &str,
) -> ResourceGroupType {
    // Format: vendor.package.namespace.type.vMAJOR -- 5 tokens per ADR-001
    // Finding 2. Suffix goes in namespace (lowercased so callers can use
    // CamelCase without breaking the GTS regex); UUID-hex (prefixed with
    // `i` so it starts with a letter) goes in type.
    let code = format!(
        "gts.cf.core.rg.type.v1~x.test.{}.i{}.v1~",
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    svc.create_type(CreateTypeRequest {
        code,
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        metadata_schema: None,
    })
    .await
    .expect("create root type")
}

/// Create a child type with specified allowed parents and memberships.
pub async fn create_child_type(
    svc: &TypeService<TypeRepository>,
    suffix: &str,
    parents: &[&str],
    memberships: &[&str],
) -> ResourceGroupType {
    // Format: vendor.package.namespace.type.vMAJOR -- 5 tokens per ADR-001
    // Finding 2. Suffix goes in namespace (lowercased so callers can use
    // CamelCase without breaking the GTS regex); UUID-hex (prefixed with
    // `i` so it starts with a letter) goes in type.
    let code = format!(
        "gts.cf.core.rg.type.v1~x.test.{}.i{}.v1~",
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    );
    svc.create_type(CreateTypeRequest {
        code,
        can_be_root: false,
        allowed_parent_types: parents.iter().map(|s| (*s).to_owned()).collect(),
        allowed_membership_types: memberships.iter().map(|s| (*s).to_owned()).collect(),
        metadata_schema: None,
    })
    .await
    .expect("create child type")
}

// -- Group helpers --

/// Create a root group of the given type.
pub async fn create_root_group(
    svc: &GroupService<GroupRepository, TypeRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    name: &str,
    tenant_id: Uuid,
) -> ResourceGroupModel {
    svc.create_group(
        ctx,
        CreateGroupRequest {
            id: None,
            code: type_code.to_owned(),
            name: name.to_owned(),
            parent_id: None,
            metadata: None,
        },
        tenant_id,
    )
    .await
    .expect("create root group")
}

/// Create a child group under the given parent.
pub async fn create_child_group(
    svc: &GroupService<GroupRepository, TypeRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    parent_id: Uuid,
    name: &str,
    tenant_id: Uuid,
) -> ResourceGroupModel {
    svc.create_group(
        ctx,
        CreateGroupRequest {
            id: None,
            code: type_code.to_owned(),
            name: name.to_owned(),
            parent_id: Some(parent_id),
            metadata: None,
        },
        tenant_id,
    )
    .await
    .expect("create child group")
}

// -- Closure table assertions --

/// Assert that the closure table contains exactly the expected (ancestor, depth)
/// pairs for a given descendant.
#[allow(clippy::disallowed_methods)]
pub async fn assert_closure_rows(
    conn: &impl toolkit_db::secure::DBRunner,
    descendant_id: Uuid,
    expected: &[(Uuid, i32)],
) {
    use resource_group::infra::storage::entity::resource_group_closure::{
        Column, Entity as ClosureEntity,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureEntityExt;

    let scope = toolkit_security::AccessScope::allow_all();
    let rows = ClosureEntity::find()
        .filter(Column::DescendantId.eq(descendant_id))
        .secure()
        .scope_with(&scope)
        .all(conn)
        .await
        .expect("query closure table");

    let mut actual: Vec<(Uuid, i32)> = rows.iter().map(|r| (r.ancestor_id, r.depth)).collect();
    actual.sort_by_key(|(id, d)| (*id, *d));

    let mut exp: Vec<(Uuid, i32)> = expected.to_vec();
    exp.sort_by_key(|(id, d)| (*id, *d));

    assert_eq!(
        actual, exp,
        "Closure rows for descendant {descendant_id} mismatch.\n  actual:   {actual:?}\n  expected: {exp:?}"
    );
}

/// Assert that the total number of closure rows for a set of group IDs
/// matches the expected count.
#[allow(clippy::disallowed_methods)]
pub async fn assert_closure_count(
    conn: &impl toolkit_db::secure::DBRunner,
    group_ids: &[Uuid],
    expected_total: usize,
) {
    use resource_group::infra::storage::entity::resource_group_closure::{
        Column, Entity as ClosureEntity,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureEntityExt;

    let scope = toolkit_security::AccessScope::allow_all();
    let rows = ClosureEntity::find()
        .filter(Column::DescendantId.is_in(group_ids.iter().copied()))
        .secure()
        .scope_with(&scope)
        .all(conn)
        .await
        .expect("query closure table");

    assert_eq!(
        rows.len(),
        expected_total,
        "Expected {expected_total} closure rows for groups {group_ids:?}, got {}",
        rows.len()
    );
}

/// Assert that a JSON value contains no surrogate integer IDs
/// (e.g. `gts_type_id` SMALLINT fields should not leak to the API).
pub fn assert_no_surrogate_ids(json: &serde_json::Value) {
    let text = json.to_string();
    assert!(
        !text.contains("gts_type_id"),
        "JSON should not contain surrogate 'gts_type_id': {text}"
    );
    assert!(
        !text.contains("schema_id"),
        "JSON should not contain surrogate 'schema_id': {text}"
    );
}

// -- Service construction helpers --

/// Build a `GroupService` from a DB provider using the allow-all enforcer.
pub fn make_group_service(
    db: Arc<DBProvider<DbError>>,
) -> GroupService<GroupRepository, TypeRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        make_enforcer(),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        make_types_registry(),
    )
}

/// Build a `MembershipService` from a DB provider using the allow-all enforcer.
pub fn make_membership_service(
    db: Arc<DBProvider<DbError>>,
) -> MembershipService<GroupRepository, TypeRepository, MembershipRepository> {
    MembershipService::new(
        db,
        make_enforcer(),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        Arc::new(MembershipRepository),
    )
}

/// Build a `GroupService` wired with the deny-all enforcer. Scoped operations
/// would be rejected; an unscoped read that still succeeds proves the AuthZ bypass.
pub fn make_group_service_deny(
    db: Arc<DBProvider<DbError>>,
) -> GroupService<GroupRepository, TypeRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        make_types_registry(),
    )
}

/// Build a `MembershipService` wired with the deny-all enforcer (see
/// [`make_group_service_deny`]).
pub fn make_membership_service_deny(
    db: Arc<DBProvider<DbError>>,
) -> MembershipService<GroupRepository, TypeRepository, MembershipRepository> {
    MembershipService::new(
        db,
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        Arc::new(MembershipRepository),
    )
}
