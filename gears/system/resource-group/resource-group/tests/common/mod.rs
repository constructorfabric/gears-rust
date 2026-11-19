// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
#![allow(dead_code, clippy::expect_used, clippy::doc_markdown)]
//! Shared test helpers for resource-group integration tests.
//!
//! Provides database setup, service construction, security context helpers,
//! and assertion utilities. Used by Phases 2-5 of the unit test plan.

use std::sync::Arc;
use toolkit_gts::gts_id;

use async_trait::async_trait;
use uuid::Uuid;

use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::DenyReason,
};
use sea_orm_migration::MigratorTrait;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::test_support::QueryRecorder;
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};

use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::membership_service::MembershipService;
use resource_group::domain::type_catalog::RgTypeCatalog;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::types_registry::RegistryTypeCatalog;
use resource_group_sdk::{CreateGroupRequest, ResourceGroup as ResourceGroupModel};
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

// -- RG types: published to a test types registry --
//
// Resource-group keeps no type definitions; its types live in the types registry.
// Tests publish them into one process-wide `MockTypesRegistry` (which
// materializes `effective_traits` and `resolved_schema` with gts-rust), seeded
// with the RG SDK's base type. Codes are unique per test, so sharing it across
// a test binary is safe.

const RG_BASE: &str = gts_id!("cf.core.rg.type.v1~");

static REGISTRY: std::sync::LazyLock<Arc<MockTypesRegistry>> = std::sync::LazyLock::new(|| {
    let registry = MockTypesRegistry::new();
    let (_, base) = resource_group_sdk::gts_declarations()
        .into_iter()
        .find(|(id, _)| id == RG_BASE)
        .expect("the RG SDK declares its base type");
    registry.seed(RG_BASE, serde_json::from_str(&base).expect("base JSON"));
    let registry = Arc::new(registry);
    // The default tenant type (resource-group's default `tenant_type_paths`),
    // the base of the tenant types tests derive.
    seed_into(
        &registry,
        &CreateTypeRequest {
            code: resource_group_sdk::TENANT_RG_TYPE_PATH.to_owned(),
            can_be_root: true,
            ..CreateTypeRequest::default()
        },
    );
    registry
});

/// The shared test types registry.
pub fn types_registry() -> Arc<MockTypesRegistry> {
    REGISTRY.clone()
}

/// `RgTypeCatalog` over the shared test types registry, as production wires it.
pub fn make_types_catalog() -> Arc<dyn RgTypeCatalog> {
    Arc::new(RegistryTypeCatalog::new(types_registry()))
}

/// `RgTypeCatalog` over a dedicated registry whose reads fail with `Internal`,
/// for tests about what runs before a registry read.
pub fn make_unavailable_types_catalog() -> Arc<dyn RgTypeCatalog> {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(
        toolkit_canonical_errors::CanonicalError::internal("types registry unavailable").create(),
    ));
    Arc::new(RegistryTypeCatalog::new(Arc::new(registry)))
}

/// An RG type as a test declares it.
#[derive(Debug, Clone, Default)]
pub struct CreateTypeRequest {
    pub code: String,
    pub can_be_root: bool,
    pub allowed_parent_types: Vec<String>,
    pub allowed_membership_types: Vec<String>,
    /// JSON Schema of the group's `metadata`; `None` leaves it unconstrained.
    pub metadata_schema: Option<serde_json::Value>,
}

/// A replacement declaration of an already published test type.
#[derive(Debug, Clone, Default)]
pub struct UpdateTypeRequest {
    pub can_be_root: bool,
    pub allowed_parent_types: Vec<String>,
    pub allowed_membership_types: Vec<String>,
    pub metadata_schema: Option<serde_json::Value>,
}

/// A published test type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceGroupType {
    pub code: String,
    pub can_be_root: bool,
    pub allowed_parent_types: Vec<String>,
    pub allowed_membership_types: Vec<String>,
    pub metadata_schema: Option<serde_json::Value>,
}

/// Publishes RG types to the shared test registry.
#[derive(Clone, Copy, Default)]
pub struct TestTypes;

// `async` keeps the call shape fixtures had against the removed RG type
// service; publishing to the in-memory registry itself does not await.
#[allow(clippy::unused_async)]
impl TestTypes {
    /// Publish `req` as a type derived from the RG base (unconditionally:
    /// the registry, not resource-group, validates type definitions).
    pub async fn create_type(
        &self,
        _ctx: &SecurityContext,
        req: CreateTypeRequest,
    ) -> Result<ResourceGroupType, String> {
        Ok(publish_type(req))
    }

    /// As [`Self::create_type`].
    pub async fn create_type_unscoped(
        &self,
        req: CreateTypeRequest,
    ) -> Result<ResourceGroupType, String> {
        Ok(publish_type(req))
    }

    /// Republish `code` with a new definition, as its owner would.
    pub async fn update_type_unscoped(
        &self,
        code: &str,
        req: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, String> {
        Ok(publish_type(CreateTypeRequest {
            code: code.to_owned(),
            can_be_root: req.can_be_root,
            allowed_parent_types: req.allowed_parent_types,
            allowed_membership_types: req.allowed_membership_types,
            metadata_schema: req.metadata_schema,
        }))
    }

    /// As [`Self::update_type_unscoped`].
    pub async fn update_type(
        &self,
        _ctx: &SecurityContext,
        code: &str,
        req: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, String> {
        self.update_type_unscoped(code, req).await
    }
}

/// Seed `req` into the shared test registry.
pub fn publish_type(req: CreateTypeRequest) -> ResourceGroupType {
    seed_into(&types_registry(), &req);
    ResourceGroupType {
        code: req.code,
        can_be_root: req.can_be_root,
        allowed_parent_types: req.allowed_parent_types,
        allowed_membership_types: req.allowed_membership_types,
        metadata_schema: req.metadata_schema,
    }
}

/// The schema a test type is published with: derived from its parent in the
/// GTS chain (the RG base for a two-segment code).
fn seed_into(registry: &MockTypesRegistry, req: &CreateTypeRequest) {
    let extension = req.metadata_schema.as_ref().map_or_else(
        || serde_json::json!({}),
        |metadata| serde_json::json!({ "properties": { "metadata": metadata } }),
    );
    let parent = {
        let trimmed = req.code.trim_end_matches('~');
        let cut = trimmed.rfind('~').expect("a derived RG type code");
        format!("{}~", &trimmed[..cut])
    };
    registry.seed(
        &req.code,
        serde_json::json!({
            "$id": format!("gts://{}", req.code),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": format!("gts://{parent}") }, extension],
            "x-gts-traits": {
                "can_be_root": req.can_be_root,
                "allowed_parent_types": req.allowed_parent_types,
                "allowed_membership_types": req.allowed_membership_types,
            },
        }),
    );
}

/// Publish a plain (non-RG) GTS type, e.g. an external member resource type.
pub fn publish_plain_type(code: &str) {
    types_registry().seed(
        code,
        serde_json::json!({
            "$id": format!("gts://{code}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        }),
    );
}

/// Mark a published type deleted; the registry keeps its content.
pub fn delete_published_type(code: &str) {
    let registry = types_registry();
    let content = registry.content(code).expect("published");
    registry.seed_entity(Seed::new(code, content).deleted());
}

// -- AllowAll AuthZ mock --

struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
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
impl AuthZResolverApi for DenyAllAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
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

/// Build a `PolicyEnforcer` whose PDP denies every request.
pub fn make_deny_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyAllAuthZ))
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

/// Insert a membership directly through the repository, in its own
/// transaction, bypassing the membership service's checks.
pub async fn insert_membership_directly(
    db: &DBProvider<DbError>,
    group_id: Uuid,
    resource_type: &str,
    resource_id: &str,
) -> Result<(), resource_group::domain::error::DomainError> {
    use resource_group::domain::repo::MembershipRepositoryTrait;
    let resource_type = resource_type.to_owned();
    let resource_id = resource_id.to_owned();
    db.db()
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                MembershipRepository
                    .insert(tx, group_id, &resource_type, &resource_id)
                    .await
            })
        })
        .await
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

/// Build a `PolicyEnforcer` whose PDP always permits and scopes to the
/// subject's tenant.
pub fn make_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverApi> = Arc::new(AllowAllAuthZ);
    PolicyEnforcer::new(authz)
}

// -- Type helpers --

/// Create a root type (can_be_root = true, no parents, no memberships).
pub async fn create_root_type(svc: &TestTypes, suffix: &str) -> ResourceGroupType {
    // Format: vendor.package.namespace.type.vMAJOR -- 5 tokens per ADR-001
    // Finding 2. Suffix goes in namespace (lowercased so callers can use
    // CamelCase without breaking the GTS regex); UUID-hex (prefixed with
    // `i` so it starts with a letter) goes in type.
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase().replace('-', "_"),
        Uuid::now_v7().as_simple()
    );
    svc.create_type_unscoped(CreateTypeRequest {
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
    svc: &TestTypes,
    suffix: &str,
    parents: &[&str],
    memberships: &[&str],
) -> ResourceGroupType {
    // Format: vendor.package.namespace.type.vMAJOR -- 5 tokens per ADR-001
    // Finding 2. Suffix goes in namespace (lowercased so callers can use
    // CamelCase without breaking the GTS regex); UUID-hex (prefixed with
    // `i` so it starts with a letter) goes in type.
    let code = format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase().replace('-', "_"),
        Uuid::now_v7().as_simple()
    );
    svc.create_type_unscoped(CreateTypeRequest {
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
    svc: &GroupService<GroupRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    name: &str,
    tenant_id: Uuid,
) -> ResourceGroupModel {
    svc.create_group(
        ctx,
        CreateGroupRequest::new(type_code.to_owned(), name.to_owned()),
        tenant_id,
    )
    .await
    .expect("create root group")
}

/// Create a child group under the given parent.
pub async fn create_child_group(
    svc: &GroupService<GroupRepository>,
    ctx: &SecurityContext,
    type_code: &str,
    parent_id: Uuid,
    name: &str,
    tenant_id: Uuid,
) -> ResourceGroupModel {
    svc.create_group(
        ctx,
        CreateGroupRequest::new(type_code.to_owned(), name.to_owned())
            .with_parent_id(Some(parent_id)),
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

/// Assert that `resource_group_closure` is exactly the transitive closure of
/// the `parent_id` links in `resource_group`, depths included.
///
/// The strongest statement available about the closure table, and the one the
/// concurrency analysis asks for: not "the rebuild wrote the rows we expected"
/// but "whatever the rebuild wrote agrees with the hierarchy the group rows
/// themselves describe". A rebuild that drops a path, leaves a stale one
/// behind, or is off by one on depth fails here no matter which code path
/// produced it -- a row-by-row loop, a batched insert, or an `INSERT SELECT`.
#[allow(clippy::disallowed_methods)]
pub async fn assert_closure_matches_parent_links(conn: &impl toolkit_db::secure::DBRunner) {
    use resource_group::infra::storage::entity::resource_group::Entity as GroupEntity;
    use resource_group::infra::storage::entity::resource_group_closure::Entity as ClosureEntity;
    use sea_orm::EntityTrait;
    use std::collections::{BTreeSet, HashMap};
    use toolkit_db::secure::SecureEntityExt;

    let scope = toolkit_security::AccessScope::allow_all();
    let groups = GroupEntity::find()
        .secure()
        .scope_with(&scope)
        .all(conn)
        .await
        .expect("query groups");
    let rows = ClosureEntity::find()
        .secure()
        .scope_with(&scope)
        .all(conn)
        .await
        .expect("query closure table");

    let parent_of: HashMap<Uuid, Option<Uuid>> =
        groups.iter().map(|g| (g.id, g.parent_id)).collect();

    // Walk every node up to its root; each step is one closure row that must
    // exist, at the distance walked. The self-row is the zeroth step.
    let mut expected: BTreeSet<(Uuid, Uuid, i32)> = BTreeSet::new();
    for g in &groups {
        let mut ancestor = Some(g.id);
        let mut depth = 0_i32;
        while let Some(a) = ancestor {
            expected.insert((a, g.id, depth));
            ancestor = parent_of.get(&a).copied().flatten();
            depth += 1;
            assert!(
                depth <= i32::try_from(groups.len()).unwrap_or(i32::MAX),
                "parent_id links form a cycle reachable from {}",
                g.id
            );
        }
    }

    let actual: BTreeSet<(Uuid, Uuid, i32)> = rows
        .iter()
        .map(|r| (r.ancestor_id, r.descendant_id, r.depth))
        .collect();

    // This helper's contract is "the closure table is *exactly* the transitive
    // closure", and a set comparison cannot see a duplicated row. Today the
    // schema makes one impossible -- `PRIMARY KEY (ancestor_id,
    // descendant_id)`, declared in both backend branches of
    // `m20260306_000001_initial.rs` (:72 Postgres, :149 SQLite) -- so this
    // cannot fire. It stays because the invariant lives in string SQL that no
    // compiler checks: a migration that weakened the key would otherwise
    // silently collapse duplicates into the set below and pass.
    assert_eq!(
        rows.len(),
        actual.len(),
        "closure table holds duplicate (ancestor, descendant, depth) rows"
    );

    let missing: Vec<_> = expected.difference(&actual).collect();
    let extra: Vec<_> = actual.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "closure table disagrees with the parent_id links it should mirror\n  \
         missing (ancestor, descendant, depth): {missing:?}\n  \
         unexpected: {extra:?}"
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

/// The test type publisher (kept under the old constructor name: the DB is
/// not involved, RG types live in the types registry).
pub fn make_type_service(_db: Arc<DBProvider<DbError>>) -> TestTypes {
    TestTypes
}

/// In-memory SQLite with migrations applied and a [`QueryRecorder`] attached,
/// for the DB-behaviour audit suites.
///
/// The callback goes on before the connection is wrapped, unlike [`test_db`]:
/// `DBProvider`/`Db` never hand out the inner `SeaORM` connection, so
/// attaching afterwards would miss every statement.
pub async fn test_db_with_recorder() -> (Arc<DBProvider<DbError>>, QueryRecorder) {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder("sqlite::memory:", opts)
        .await
        .expect("connect to in-memory SQLite with recorder");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    // Migrations run their own DDL through the same callback -- start each
    // test's trace from a clean slate.
    recorder.clear();

    (Arc::new(DBProvider::new(db)), recorder)
}

/// Build a `GroupService` from a DB provider using the permitting,
/// tenant-scoping enforcer ([`make_enforcer`]).
pub fn make_group_service(db: Arc<DBProvider<DbError>>) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        make_enforcer(),
        Arc::new(GroupRepository),
        make_types_catalog(),
    )
}

/// Build a `MembershipService` from a DB provider using the permitting,
/// tenant-scoping enforcer ([`make_enforcer`]).
pub fn make_membership_service(
    db: Arc<DBProvider<DbError>>,
) -> MembershipService<GroupRepository, MembershipRepository> {
    MembershipService::new(
        db,
        make_enforcer(),
        Arc::new(GroupRepository),
        make_types_catalog(),
        Arc::new(MembershipRepository),
    )
}

/// Build a `GroupService` wired with the deny-all enforcer. Scoped operations
/// would be rejected; an unscoped read that still succeeds proves the AuthZ bypass.
pub fn make_group_service_deny(db: Arc<DBProvider<DbError>>) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        make_deny_enforcer(),
        Arc::new(GroupRepository),
        make_types_catalog(),
    )
}

/// Build a `MembershipService` wired with the deny-all enforcer (see
/// [`make_group_service_deny`]).
pub fn make_membership_service_deny(
    db: Arc<DBProvider<DbError>>,
) -> MembershipService<GroupRepository, MembershipRepository> {
    MembershipService::new(
        db,
        make_deny_enforcer(),
        Arc::new(GroupRepository),
        make_types_catalog(),
        Arc::new(MembershipRepository),
    )
}

/// A `GroupService` over an explicit type catalogue.
pub fn make_group_service_with(
    db: Arc<DBProvider<DbError>>,
    types: Arc<dyn RgTypeCatalog>,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        make_enforcer(),
        Arc::new(GroupRepository),
        types,
    )
}

/// A `MembershipService` over an explicit type catalogue.
pub fn make_membership_service_with(
    db: Arc<DBProvider<DbError>>,
    types: Arc<dyn RgTypeCatalog>,
) -> MembershipService<GroupRepository, MembershipRepository> {
    MembershipService::new(
        db,
        make_enforcer(),
        Arc::new(GroupRepository),
        types,
        Arc::new(MembershipRepository),
    )
}
