// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
#![allow(dead_code, clippy::expect_used, clippy::doc_markdown)]
//! Shared test helpers for resource-group integration tests.
//!
//! Provides database setup, service construction, security context helpers,
//! the Types Registry fixture ([`TestTypes`]) and assertion utilities. Used
//! by Phases 2-5 of the unit test plan.

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
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::membership_service::MembershipService;
use resource_group::domain::type_catalog::TypeCatalog;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;
use resource_group::infra::type_catalog::TrTypeCatalog;
use resource_group_sdk::{CreateGroupRequest, ResourceGroup as ResourceGroupModel};

// -- Types Registry fixture --

/// A resource-group type registered in the mock Types Registry, as the tests
/// need to read it back.
#[derive(Debug, Clone)]
pub struct TestType {
    pub code: String,
    pub can_be_root: bool,
    pub allowed_parent_types: Vec<String>,
    pub allowed_membership_types: Vec<String>,
}

/// Types Registry fixture: a [`MockTypesRegistry`] seeded with the RG base
/// type and the static tenant type, plus helpers to register test types.
///
/// One fixture serves one test; services built from it
/// (`make_group_service(db, &types)` ...) share its catalog.
pub struct TestTypes {
    pub mock: Arc<MockTypesRegistry>,
}

impl Default for TestTypes {
    fn default() -> Self {
        Self::new()
    }
}

impl TestTypes {
    /// A registry holding the RG base type and the static tenant type.
    #[must_use]
    pub fn new() -> Self {
        let mock = Arc::new(MockTypesRegistry::new());
        mock.seed_inventory();
        Self { mock }
    }

    /// The catalog the services read types through.
    #[must_use]
    pub fn catalog(&self) -> Arc<dyn TypeCatalog> {
        Arc::new(TrTypeCatalog::new(self.mock.clone()))
    }

    /// Registers a derived RG type and returns its code.
    ///
    /// `metadata_schema` becomes the schema of the group `metadata`; `None`
    /// means a free-form object.
    pub fn add_rg_type(
        &self,
        code: &str,
        can_be_root: bool,
        parents: &[&str],
        memberships: &[&str],
        metadata_schema: Option<serde_json::Value>,
    ) -> String {
        let metadata = metadata_schema.unwrap_or_else(|| serde_json::json!({"type": "object"}));
        let doc = serde_json::json!({
            "$id": format!("gts://{code}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "x-gts-traits": {
                "can_be_root": can_be_root,
                "allowed_parent_types": parents,
                "allowed_membership_types": memberships,
            },
            "allOf": [
                { "$ref": format!("gts://{}", parent_type_code(code)) },
                { "type": "object", "properties": { "metadata": metadata } }
            ]
        });
        self.mock.seed(code, doc);
        code.to_owned()
    }

    /// Registers a plain (non-RG) GTS type usable as a membership resource
    /// type and returns its code.
    pub fn add_member_type(&self, code: &str) -> String {
        let doc = serde_json::json!({
            "$id": format!("gts://{code}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        });
        self.mock.seed(code, doc);
        code.to_owned()
    }

    /// The Registry Reference (UUID) the registry reports for a type code, as
    /// stored in the `gts_type_id` columns.
    ///
    /// # Panics
    /// If the type is not registered.
    pub async fn uuid_of(&self, code: &str) -> Uuid {
        let defs = self
            .catalog()
            .resolve(&[code.to_owned()], &[])
            .await
            .expect("catalog read")
            .by_code;
        defs.get(code)
            .unwrap_or_else(|| panic!("type {code} is not registered"))
            .uuid
    }

    /// Registers a fresh plain member type with a unique code.
    pub fn new_member_type(&self, suffix: &str) -> String {
        self.add_member_type(&unique_member_code(suffix))
    }

    /// Makes every subsequent catalog read fail (Types Registry unavailable).
    pub fn fail_reads(&self) {
        self.mock
            .inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    }

    /// Number of catalog reads (`batch_get`) issued so far.
    #[must_use]
    pub fn batch_get_calls(&self) -> u32 {
        self.mock.calls(Call::BatchGet)
    }
}

/// The direct parent of a chained GTS type code (the code without its last
/// segment): `base~x.v1~` -> `base~`.
fn parent_type_code(code: &str) -> &str {
    let body = code.strip_suffix('~').unwrap_or(code);
    body.rfind('~').map_or(code, |i| &code[..=i])
}

/// A unique RG type code derived from the RG base type.
///
/// Format: vendor.package.namespace.type.vMAJOR -- 5 tokens per ADR-001
/// Finding 2. Suffix goes in namespace (lowercased so callers can use
/// CamelCase without breaking the GTS regex); UUID-hex (prefixed with `i` so
/// it starts with a letter) goes in type.
#[must_use]
pub fn unique_rg_code(suffix: &str) -> String {
    format!(
        "{}x.test.{}.i{}.v1~",
        gts_id!("cf.core.rg.type.v1~"),
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    )
}

/// A unique single-segment code for a plain member type.
#[must_use]
pub fn unique_member_code(suffix: &str) -> String {
    format!(
        "gts.x.test.{}.i{}.v1~",
        suffix.to_ascii_lowercase(),
        Uuid::now_v7().as_simple()
    )
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
    let authz: Arc<dyn AuthZResolverApi> = Arc::new(AllowAllAuthZ);
    PolicyEnforcer::new(authz)
}

// -- Type helpers --

/// Register a root type (can_be_root = true, no parents, no memberships).
pub fn create_root_type(types: &TestTypes, suffix: &str) -> TestType {
    let code = types.add_rg_type(&unique_rg_code(suffix), true, &[], &[], None);
    TestType {
        code,
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
    }
}

/// Register a child type (can_be_root = false) with the given allowed
/// parents and memberships.
pub fn create_child_type(
    types: &TestTypes,
    suffix: &str,
    parents: &[&str],
    memberships: &[&str],
) -> TestType {
    let code = types.add_rg_type(&unique_rg_code(suffix), false, parents, memberships, None);
    TestType {
        code,
        can_be_root: false,
        allowed_parent_types: parents.iter().map(|s| (*s).to_owned()).collect(),
        allowed_membership_types: memberships.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Register a root type that accepts the given membership resource types.
pub fn create_root_type_with_memberships(
    types: &TestTypes,
    suffix: &str,
    memberships: &[&str],
) -> TestType {
    let code = types.add_rg_type(&unique_rg_code(suffix), true, &[], memberships, None);
    TestType {
        code,
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: memberships.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Register a root-capable type that allows itself as a parent, so a chain of
/// arbitrary depth can be built.
pub fn create_self_parenting_type(types: &TestTypes, suffix: &str) -> TestType {
    let code = unique_rg_code(suffix);
    types.add_rg_type(&code, true, &[&code], &[], None);
    TestType {
        code: code.clone(),
        can_be_root: true,
        allowed_parent_types: vec![code],
        allowed_membership_types: vec![],
    }
}

/// A unique tenant-type code: it starts with `TENANT_RG_TYPE_PATH`, so groups
/// of it are classified as tenant groups (`is_tenant`).
#[must_use]
pub fn unique_tenant_type_code() -> String {
    format!(
        "{}x.test.tn.i{}.v1~",
        resource_group_sdk::TENANT_RG_TYPE_PATH,
        Uuid::now_v7().as_simple()
    )
}

/// Register a root-capable tenant-derived type (no allowed parents).
pub fn create_tenant_type(types: &TestTypes) -> TestType {
    let code = types.add_rg_type(&unique_tenant_type_code(), true, &[], &[], None);
    TestType {
        code,
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
    }
}

/// Register a root-capable tenant-derived type that may be placed under the
/// given parent tenant type.
pub fn create_tenant_sub_type(types: &TestTypes, parent_code: &str) -> TestType {
    let code = types.add_rg_type(&unique_tenant_type_code(), true, &[parent_code], &[], None);
    TestType {
        code,
        can_be_root: true,
        allowed_parent_types: vec![parent_code.to_owned()],
        allowed_membership_types: vec![],
    }
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
    // `m20261012_000001_types_in_types_registry.rs` (:67 Postgres, :129 SQLite) -- so this
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

/// Assert that a JSON value contains no internal identifiers: the
/// `gts_type_id` column holds a Registry Reference UUID and `is_tenant` is a
/// storage flag; neither may leak to the API.
pub fn assert_no_surrogate_ids(json: &serde_json::Value) {
    let text = json.to_string();
    assert!(
        !text.contains("gts_type_id"),
        "JSON should not contain internal 'gts_type_id': {text}"
    );
    assert!(
        !text.contains("is_tenant"),
        "JSON should not contain internal 'is_tenant': {text}"
    );
}

// -- Service construction helpers --

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

/// Build a `GroupService` from a DB provider using the allow-all enforcer.
pub fn make_group_service(
    db: Arc<DBProvider<DbError>>,
    types: &TestTypes,
) -> GroupService<GroupRepository> {
    make_group_service_with_profile(db, types, QueryProfile::default())
}

/// Build a `GroupService` with a custom `QueryProfile` (allow-all enforcer).
pub fn make_group_service_with_profile(
    db: Arc<DBProvider<DbError>>,
    types: &TestTypes,
    profile: QueryProfile,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        profile,
        make_enforcer(),
        Arc::new(GroupRepository),
        types.catalog(),
    )
}

/// Build a `MembershipService` from a DB provider using the allow-all enforcer.
pub fn make_membership_service(
    db: Arc<DBProvider<DbError>>,
    types: &TestTypes,
) -> MembershipService<GroupRepository, MembershipRepository> {
    MembershipService::new(
        db,
        make_enforcer(),
        Arc::new(GroupRepository),
        types.catalog(),
        Arc::new(MembershipRepository),
    )
}

/// Build a `GroupService` wired with the deny-all enforcer. Scoped operations
/// would be rejected; an unscoped read that still succeeds proves the AuthZ bypass.
pub fn make_group_service_deny(
    db: Arc<DBProvider<DbError>>,
    types: &TestTypes,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
        Arc::new(GroupRepository),
        types.catalog(),
    )
}

/// Build a `MembershipService` wired with the deny-all enforcer (see
/// [`make_group_service_deny`]).
pub fn make_membership_service_deny(
    db: Arc<DBProvider<DbError>>,
    types: &TestTypes,
) -> MembershipService<GroupRepository, MembershipRepository> {
    MembershipService::new(
        db,
        PolicyEnforcer::new(Arc::new(DenyAllAuthZ)),
        Arc::new(GroupRepository),
        types.catalog(),
        Arc::new(MembershipRepository),
    )
}
