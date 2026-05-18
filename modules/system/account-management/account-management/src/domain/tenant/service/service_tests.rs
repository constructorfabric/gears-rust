//! In-file unit tests with in-memory `FakeTenantRepo` and
//! `FakeIdpProvisioner`. All tests are hermetic -- no DB, no network,
//! no filesystem.

use account_management_sdk::TenantStatus as PublicTenantStatus;
use modkit_odata::ODataQuery;
use modkit_odata::ast::{CompareOperator, Expr, Value as OdataValue};

use super::*;

/// Build a one-predicate `$filter=status eq '<label>'` `ODataQuery`
/// for `list_children` tests. Constructing the AST by hand keeps the
/// test dependency surface stable — relying on the parser would couple
/// these tests to grammar changes in `modkit-odata`. The string form
/// matches the public SDK contract; the impl-side
/// `TenantODataMapper::map_value` translates it into the storage
/// SMALLINT before binding.
fn list_children_status_eq(status: PublicTenantStatus) -> ODataQuery {
    let label = match status {
        PublicTenantStatus::Active => "active",
        PublicTenantStatus::Suspended => "suspended",
        PublicTenantStatus::Deleted => "deleted",
    };
    let expr = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::String(label.to_owned()))),
    );
    ODataQuery::default().with_filter(expr).with_limit(10)
}
use crate::config::AccountManagementConfig;
use crate::domain::tenant::closure::ClosureRow;
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::resource_checker::InertResourceOwnershipChecker;
use crate::domain::tenant::test_support::{
    FakeDeprovisionOutcome, FakeIdpProvisioner, FakeOutcome, FakeTenantRepo,
    constraint_bearing_enforcer, mock_enforcer,
};
use async_trait::async_trait;
use modkit_security::AccessScope;
use std::sync::Mutex;
use time::OffsetDateTime;

/// Test-only `TypesRegistryClient` that resolves every UUID to the
/// same hard-coded chained `GtsTypeSchema`. The reaper / retention
/// pipelines (and `UserService::resolve_active_tenant`) call
/// `get_type_schema_by_uuid` on the tenant's `tenant_type_uuid`
/// before `IdpPluginClient::deprovision_tenant`; the production code
/// surfaces a miss as `ServiceUnavailable`, but the in-memory test
/// repo seeds an opaque `Uuid::from_u128(0xAA)` that no real
/// registry would know about. Returning the canonical
/// `gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~` schema for
/// any UUID keeps the existing fixtures viable without touching every
/// `with_root` / `seed_tenant` call site — the precise UUID is not
/// load-bearing for these tests, only the typed `tenant_type` on the
/// resulting `TenantContext`.
struct ConstantTypesRegistry;

fn synth_type_schema(type_id: &str) -> types_registry_sdk::GtsTypeSchema {
    let parent = types_registry_sdk::GtsTypeSchema::derive_parent_type_id(type_id)
        .map(|p| std::sync::Arc::new(synth_type_schema(p.as_ref())));
    types_registry_sdk::GtsTypeSchema::try_new(
        types_registry_sdk::GtsTypeId::new(type_id),
        serde_json::json!({}),
        None,
        parent,
    )
    .expect("synthetic GtsTypeSchema for test is valid")
}

#[async_trait]
impl types_registry_sdk::TypesRegistryClient for ConstantTypesRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn register_type_schemas(
        &self,
        _schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn get_type_schema(
        &self,
        type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError> {
        // Surface NOT_FOUND for AM-owned resource schemas
        // (`gts.cf.core.am.tenant.v1~` / `.user.v1~`) so callers like
        // `validate_tenant_name_via_gts` short-circuit to `Ok(())`
        // and the AM-side `trim+empty` / DB `CHECK` constraints
        // remain the authoritative gate in tests. The
        // `get_type_schema_by_uuid` path below still returns a
        // synthetic schema because `load_tenant_context` treats
        // missing entries as a hard failure (`ServiceUnavailable`)
        // after the IdP-metadata isolation refactor.
        Err(types_registry_sdk::TypesRegistryError::gts_type_schema_not_found(type_id))
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError> {
        Ok(synth_type_schema(
            "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~",
        ))
    }
    async fn get_type_schemas(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn get_instance(
        &self,
        _id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError> {
        Err(types_registry_sdk::TypesRegistryError::Internal(
            "not implemented for constant test fake".into(),
        ))
    }
    async fn get_instance_by_uuid(
        &self,
        _uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError> {
        Err(types_registry_sdk::TypesRegistryError::Internal(
            "not implemented for constant test fake".into(),
        ))
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, types_registry_sdk::TypesRegistryError> {
        Ok(Vec::new())
    }
}

/// Stub registry where `get_type_schema_by_uuid` always returns
/// `GtsTypeSchemaNotFound`. Drives the H1 fix's catalog-drift
/// fallback path on `load_tenant_context`.
struct UuidNotFoundTypesRegistry;

#[async_trait]
impl types_registry_sdk::TypesRegistryClient for UuidNotFoundTypesRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn register_type_schemas(
        &self,
        _schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn get_type_schema(
        &self,
        type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError> {
        Err(types_registry_sdk::TypesRegistryError::gts_type_schema_not_found(type_id))
    }
    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError> {
        Err(
            types_registry_sdk::TypesRegistryError::gts_type_schema_not_found(
                type_uuid.as_simple().to_string(),
            ),
        )
    }
    async fn get_type_schemas(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsTypeSchema, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, types_registry_sdk::TypesRegistryError>
    {
        Ok(Vec::new())
    }
    async fn get_instance(
        &self,
        _id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError> {
        Err(types_registry_sdk::TypesRegistryError::Internal(
            "not implemented for uuid-not-found fake".into(),
        ))
    }
    async fn get_instance_by_uuid(
        &self,
        _uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError> {
        Err(types_registry_sdk::TypesRegistryError::Internal(
            "not implemented for uuid-not-found fake".into(),
        ))
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _ids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsInstance, types_registry_sdk::TypesRegistryError>,
    > {
        std::collections::HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, types_registry_sdk::TypesRegistryError> {
        Ok(Vec::new())
    }
}

fn ctx_for(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(tenant_id)
        .build()
        .expect("ctx")
}

// -----------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------

fn make_service(repo: Arc<FakeTenantRepo>, outcome: FakeOutcome) -> TenantService<FakeTenantRepo> {
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(outcome)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(Arc::new(ConstantTypesRegistry))
}

/// Variant of [`make_service`] with `integrity_check.repair.enabled
/// = true` so the repair-side service tests can drive the
/// `repair_hierarchy_integrity` path without being blocked by the
/// staged-rollout feature switch (default-off).
fn make_service_repair_enabled(
    repo: Arc<FakeTenantRepo>,
    outcome: FakeOutcome,
) -> TenantService<FakeTenantRepo> {
    let mut cfg = AccountManagementConfig::default();
    cfg.integrity_check.repair.enabled = true;
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(outcome)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
    .with_types_registry(Arc::new(ConstantTypesRegistry))
}

fn child_input(child_id: Uuid, parent_id: Uuid) -> CreateTenantRequest {
    CreateTenantRequest::new(
        child_id,
        parent_id,
        "child",
        gts::GtsSchemaId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"),
    )
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[tokio::test]
async fn create_tenant_happy_path_writes_self_row_and_one_ancestor_row() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x200);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);

    let created = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create ok");

    assert_eq!(created.id.0, child);
    assert_eq!(created.status, account_management_sdk::TenantStatus::Active);
    // `depth` is also on the public `Tenant` projection now — pin
    // it on the returned value directly, and additionally verify the
    // repo-side fake row to keep the storage column wired through.
    assert_eq!(created.depth, 1);
    let row = repo
        .find_by_id_unchecked(child)
        .expect("activated row in fake repo");
    assert_eq!(row.depth, 1);

    // Closure: root self-row + new child self-row + one strict-ancestor row.
    let closure = repo.snapshot_closure();
    assert_eq!(closure.len(), 3);
    assert!(
        closure
            .iter()
            .any(|r| r.ancestor_id == child && r.descendant_id == child && r.barrier == 0)
    );
    assert!(
        closure
            .iter()
            .any(|r| r.ancestor_id == root && r.descendant_id == child && r.barrier == 0)
    );
}

#[tokio::test]
async fn create_tenant_clean_failure_compensates_and_writes_no_closure_rows() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x201);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let closure_before = repo.snapshot_closure().len();
    let svc = make_service(repo.clone(), FakeOutcome::CleanFailure);

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("should fail");
    assert_eq!(err.code(), "service_unavailable");

    // Compensation removed the provisioning row.
    let tenant = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(tenant.is_none(), "provisioning row compensated");
    // No closure rows written.
    assert_eq!(repo.snapshot_closure().len(), closure_before);
}

#[tokio::test]
async fn create_tenant_ambiguous_failure_keeps_provisioning_row_and_writes_no_closure_rows() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x202);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let closure_before = repo.snapshot_closure().len();
    let svc = make_service(repo.clone(), FakeOutcome::Ambiguous);

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("should fail");
    assert_eq!(err.code(), "internal");

    // Provisioning row STILL PRESENT -- reaper will compensate asynchronously.
    let tenant = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(tenant.is_some(), "provisioning row retained");
    assert_eq!(tenant.unwrap().status, TenantStatus::Provisioning);
    assert_eq!(repo.snapshot_closure().len(), closure_before);
}

#[tokio::test]
async fn create_tenant_unsupported_op_compensates_and_surfaces_idp_unsupported_operation() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x203);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Unsupported);

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("should fail");
    assert_eq!(err.code(), "unsupported_operation");
    assert!(
        repo.find_by_id(&AccessScope::allow_all(), child)
            .await
            .expect("repo")
            .is_none()
    );
}

#[tokio::test]
async fn create_tenant_advisory_depth_threshold_emits_metric_and_succeeds() {
    // Per `algo-depth-threshold-evaluation` the advisory branch
    // fires at `depth > threshold` and creation proceeds. We pin
    // a low `depth_threshold = 4`, build a chain of depth 0..=4,
    // and create a child under the deepest existing tenant -- the
    // child lands at depth 5 (= threshold + 1) which exceeds the
    // threshold and triggers the advisory emission *without*
    // strict-mode rejection.
    let repo = Arc::new(FakeTenantRepo::new());
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");

    let mut prev: Option<Uuid> = None;
    let mut deepest = Uuid::nil();
    for i in 0..=4u128 {
        let id = Uuid::from_u128(0x1000 + i);
        let model = TenantModel {
            id,
            parent_id: prev,
            name: format!("t{i}"),
            status: TenantStatus::Active,
            self_managed: false,
            tenant_type_uuid: Uuid::from_u128(0xAA),
            depth: u32::try_from(i).expect("u32"),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        repo.insert_tenant_raw(model);
        prev = Some(id);
        deepest = id;
    }

    let cfg = AccountManagementConfig {
        hierarchy: crate::config::HierarchyConfig {
            depth_strict_mode: false,
            depth_threshold: 4,
        },
        ..AccountManagementConfig::default()
    };
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        cfg,
        Arc::new(InertResourceOwnershipChecker),
    );
    let child = Uuid::from_u128(0x9999);
    let root = Uuid::from_u128(0x1000);
    let created = svc
        .create_tenant(&ctx_for(root), child_input(child, deepest))
        .await
        .expect("advisory branch still proceeds");
    assert_eq!(created.status, account_management_sdk::TenantStatus::Active);
    let row = repo
        .find_by_id_unchecked(child)
        .expect("activated row in fake repo");
    assert_eq!(row.depth, 5);
}

#[tokio::test]
async fn get_tenant_happy_path_returns_model() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo, FakeOutcome::Ok);
    let t = svc.get_tenant(&ctx_for(root), root).await.expect("read ok");
    assert_eq!(t.id.0, root);
}

#[tokio::test]
async fn get_tenant_not_found_returns_not_found() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo, FakeOutcome::Ok);
    let err = svc
        .get_tenant(&ctx_for(root), Uuid::from_u128(0xDEAD))
        .await
        .expect_err("should be not found");
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn get_tenant_provisioning_tenant_is_reported_as_not_found() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x201);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Insert a provisioning tenant directly.
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: child,
        parent_id: Some(root),
        name: "prov".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
    let svc = make_service(repo, FakeOutcome::Ok);
    let err = svc
        .get_tenant(&ctx_for(root), child)
        .await
        .expect_err("should hide");
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn list_children_honours_top_and_skip() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    for i in 0..5u128 {
        let child = Uuid::from_u128(0x300 + i);
        svc.create_tenant(&ctx_for(root), child_input(child, root))
            .await
            .expect("create");
    }
    let page = svc
        .list_children(&ctx_for(root), root, &ODataQuery::default().with_limit(2))
        .await
        .expect("list ok");
    // Cursor-based pagination: the first page returns exactly `top`
    // items and the `limit` round-trips on `page_info`. Offset /
    // total semantics were retired with `TenantPage<T>`; tests that
    // need to walk to the next page now drive the listing via
    // `next_cursor` (covered by `paginate_odata`'s own integration
    // tests on the real DB).
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.page_info.limit, 2);
}

#[tokio::test]
async fn list_children_status_filter_applies() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let c1 = Uuid::from_u128(0x301);
    let c2 = Uuid::from_u128(0x302);
    svc.create_tenant(&ctx_for(root), child_input(c1, root))
        .await
        .expect("c1");
    svc.create_tenant(&ctx_for(root), child_input(c2, root))
        .await
        .expect("c2");
    svc.suspend_tenant(&ctx_for(root), c2)
        .await
        .expect("suspend c2");

    // Filter by the public SDK string contract — `'active'` /
    // `'suspended'` / `'deleted'`. The impl-side
    // `TenantODataMapper::map_value` translates these into the
    // storage SMALLINT before binding.
    let active_only = svc
        .list_children(
            &ctx_for(root),
            root,
            &list_children_status_eq(PublicTenantStatus::Active),
        )
        .await
        .expect("list ok");
    assert_eq!(active_only.items.len(), 1);
    assert_eq!(active_only.items[0].id.0, c1);

    let suspended_only = svc
        .list_children(
            &ctx_for(root),
            root,
            &list_children_status_eq(PublicTenantStatus::Suspended),
        )
        .await
        .expect("list ok");
    assert_eq!(suspended_only.items.len(), 1);
    assert_eq!(suspended_only.items[0].id.0, c2);
}

#[tokio::test]
async fn update_tenant_accepts_name_then_status_transitions_via_dedicated_methods() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let child = Uuid::from_u128(0x400);
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create");

    let renamed = svc
        .update_tenant(
            &ctx_for(root),
            child,
            UpdateTenantRequest::new().with_name("renamed"),
        )
        .await
        .expect("rename ok");
    assert_eq!(renamed.name, "renamed");

    let suspended = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect("suspend ok");
    assert_eq!(suspended.status, PublicTenantStatus::Suspended);

    let reactivated = svc
        .unsuspend_tenant(&ctx_for(root), child)
        .await
        .expect("unsuspend ok");
    assert_eq!(reactivated.status, PublicTenantStatus::Active);

    // Verify descendant_status was rewritten in the closure (status denorm invariant).
    let closure = repo.snapshot_closure();
    assert!(
        closure
            .iter()
            .filter(|r| r.descendant_id == child)
            .all(|r| r.descendant_status == TenantStatus::Active.as_smallint())
    );
}

/// Idempotent same-state transition — calling `unsuspend_tenant` on
/// an already-Active tenant MUST:
/// 1. Succeed (no 409 / `failed_precondition`).
/// 2. Skip the DB UPDATE — `updated_at` unchanged.
/// 3. Skip the closure rewrite — `descendant_status` rows untouched.
///
/// The third requirement is a nice property for the closure-self-row
/// invariant: a no-op transition is observably indistinguishable
/// from a read on the `tenant_closure` side.
#[tokio::test]
async fn unsuspend_tenant_no_op_is_idempotent() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let child = Uuid::from_u128(0x420);
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create");

    let before = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("child row present");

    // Tiny pause so a `now()` re-stamp would be visible against the
    // pre-call `updated_at`. The `FakeTenantRepo` uses
    // `OffsetDateTime::now_utc()` which has sub-microsecond resolution;
    // 1 ms is plenty.
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let returned = svc
        .unsuspend_tenant(&ctx_for(root), child)
        .await
        .expect("same-status call must be idempotent ok, not 409");
    assert_eq!(returned.status, PublicTenantStatus::Active);

    let after = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("child row still present");
    assert_eq!(
        after.updated_at, before.updated_at,
        "true idempotency: no DB write, `updated_at` MUST NOT bump"
    );
    assert_eq!(after.status, before.status);
}

/// Idempotent PATCH on `name` mirrors the status path. Sending the
/// current name MUST be a no-op success without bumping `updated_at`.
/// Today's behavior accidentally rewrites the row (still succeeds, but
/// bumps the timestamp and runs a wasted DB write); this test pins the
/// post-fix idempotent contract symmetric with the status path.
#[tokio::test]
async fn update_tenant_name_no_op_is_idempotent() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let child = Uuid::from_u128(0x421);
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create");

    let before = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("child row present");
    let current_name = before.name.clone();

    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    svc.update_tenant(
        &ctx_for(root),
        child,
        UpdateTenantRequest::new().with_name(current_name),
    )
    .await
    .expect("same-name PATCH is an idempotent no-op");

    let after = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("child row still present");
    assert_eq!(
        after.updated_at, before.updated_at,
        "name no-op MUST be idempotent: no DB write, no `updated_at` bump"
    );
    assert_eq!(after.name, before.name);
}

/// Two consecutive identical `suspend_tenant` calls both succeed and
/// produce the same observable state — the canonical retry-safety
/// property an idempotent lifecycle transition promises. If the
/// response of the first request is lost in the network and the
/// client retries, they get back the same answer with no additional
/// state mutation.
#[tokio::test]
async fn suspend_tenant_double_call_is_observably_identical() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let child = Uuid::from_u128(0x422);
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create");

    // First call actually changes the status — bumps `updated_at`.
    let first = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect("first suspend ok");
    let first_row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("row present");

    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    // Second identical call is a no-op.
    let second = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect("retry of identical suspend MUST also succeed");
    let second_row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("row present");

    assert_eq!(first.status, second.status);
    assert_eq!(first.name, second.name);
    assert_eq!(
        first_row.updated_at, second_row.updated_at,
        "second identical suspend MUST NOT touch `updated_at` (idempotency)"
    );
}

#[tokio::test]
async fn update_tenant_rejects_empty_patch() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    let err = svc
        .update_tenant(&ctx_for(root), root, UpdateTenantRequest::default())
        .await
        .expect_err("reject");
    assert_eq!(err.code(), "validation");
}

// The previous `update_tenant_rejects_transition_to_deleted` test
// is no longer applicable: `UpdateTenantRequest` lost its `status`
// field, so the patch can no longer carry `Deleted` at all. Soft-
// delete idempotency on already-deleted rows is covered by
// `delete_tenant_is_idempotent_on_already_deleted_tenant` below.

#[tokio::test]
async fn unsuspend_tenant_rejects_provisioning_as_not_found() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x600);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: child,
        parent_id: Some(root),
        name: "prov".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
    let svc = make_service(repo, FakeOutcome::Ok);
    let err = svc
        .unsuspend_tenant(&ctx_for(root), child)
        .await
        .expect_err("must not see provisioning tenant");
    // Provisioning is AM-internal — the service surfaces not_found.
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn update_tenant_accepts_oversized_name_when_gts_schema_unregistered() {
    // The synchronous `validate_tenant_name` was deleted in favour
    // of `domain::gts_validation::validate_tenant_name_via_gts`,
    // which mirrors the resource-group `validate_metadata_via_gts`
    // posture: when the registry has no `gts.cf.core.am.tenant.v1~`
    // schema registered, validation short-circuits to `Ok(())` and
    // the database `CHECK (length(name) BETWEEN 1 AND 255)`
    // constraint declared by `m0001` becomes the authoritative
    // gate. The unit-level fake repo does NOT enforce DB CHECK
    // constraints, so this test pins the documented no-op
    // behaviour for the unregistered-schema path. The
    // schema-registered rejection path is exercised by the
    // integration suite, which boots the real Types Registry with
    // the AM schemas loaded.
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo, FakeOutcome::Ok);
    let info = svc
        .update_tenant(
            &ctx_for(root),
            root,
            UpdateTenantRequest::new().with_name("x".repeat(256)),
        )
        .await
        .expect("rename succeeds without registered schema; DB CHECK is the prod fence");
    assert_eq!(info.name.chars().count(), 256);
}

// ---- Closure invariant end-to-end ------------------------------

#[tokio::test]
async fn closure_invariants_are_preserved_across_self_managed_path() {
    // Layout: root(d=0,sm=false) -> mid(d=1,sm=true) -> leaf(d=2,sm=false)
    let root = Uuid::from_u128(0x100);
    let mid = Uuid::from_u128(0x110);
    let leaf = Uuid::from_u128(0x111);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);

    let mut mid_input = child_input(mid, root);
    mid_input.self_managed = true;
    svc.create_tenant(&ctx_for(root), mid_input)
        .await
        .expect("mid ok");
    svc.create_tenant(&ctx_for(root), child_input(leaf, mid))
        .await
        .expect("leaf ok");

    let closure = repo.snapshot_closure();
    // Self-row barrier invariant: every self-row has barrier=0.
    for row in closure.iter().filter(|r| r.is_self_row()) {
        assert_eq!(row.barrier, 0, "self-row barrier must be 0");
    }
    // Leaf participates in 3 rows: self + mid + root.
    let leaf_rows: Vec<_> = closure.iter().filter(|r| r.descendant_id == leaf).collect();
    assert_eq!(leaf_rows.len(), 3);
    let root_to_leaf = leaf_rows
        .iter()
        .find(|r| r.ancestor_id == root)
        .expect("root->leaf row");
    let mid_to_leaf = leaf_rows
        .iter()
        .find(|r| r.ancestor_id == mid)
        .expect("mid->leaf row");
    // Strict path from root to leaf is {mid, leaf}; mid is self-managed, so barrier=1.
    assert_eq!(
        root_to_leaf.barrier, 1,
        "self-managed mid sets barrier on root->leaf"
    );
    // Strict path from mid to leaf is {leaf}; leaf is not self-managed, so barrier=0.
    assert_eq!(mid_to_leaf.barrier, 0, "no self-managed below mid");
}

#[tokio::test]
async fn closure_invariants_no_self_managed_gives_all_zero_barriers() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x110);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo.clone(), FakeOutcome::Ok);
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("ok");

    let closure = repo.snapshot_closure();
    // Every row barrier must be 0 when no tenant on any strict path is self-managed.
    for row in &closure {
        assert_eq!(row.barrier, 0);
    }
}

#[tokio::test]
async fn create_tenant_rejects_inactive_parent() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Suspend the root via direct mutation.
    {
        let mut state = repo.state.lock().expect("lock");
        state.tenants.get_mut(&root).expect("root").status = TenantStatus::Suspended;
    }
    let svc = make_service(repo, FakeOutcome::Ok);
    let err = svc
        .create_tenant(&ctx_for(root), child_input(Uuid::from_u128(0x700), root))
        .await
        .expect_err("suspended parent rejects");
    assert_eq!(err.code(), "validation");
}

// =================================================================
// Phase 3 -- soft delete / hard delete / reaper / integrity / strict
// =================================================================

use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;
use futures::future::FutureExt;
use modkit_security::SecurityContext;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration as StdDuration;

fn ctx() -> SecurityContext {
    // Default Phase-3 test ctx -- caller is the platform admin (home
    // tenant == root tenant id used by `FakeTenantRepo::with_root`).
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(Uuid::from_u128(0x100))
        .build()
        .expect("ctx")
}

fn svc_with(
    repo: Arc<FakeTenantRepo>,
    outcome: FakeOutcome,
    cfg: AccountManagementConfig,
    checker: Arc<dyn ResourceOwnershipChecker>,
) -> TenantService<FakeTenantRepo> {
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(outcome)),
        checker,
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
    .with_types_registry(Arc::new(ConstantTypesRegistry))
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
struct CountingChecker {
    count: u64,
}
#[async_trait]
impl ResourceOwnershipChecker for CountingChecker {
    async fn count_ownership_links(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<u64, DomainError> {
        Ok(self.count)
    }
}

#[tokio::test]
async fn delete_tenant_rejects_root_tenant() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo,
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );
    let err = svc
        .delete_tenant(&ctx(), root)
        .await
        .expect_err("root reject");
    assert_eq!(err.code(), "root_tenant_cannot_delete");
}

#[tokio::test]
async fn delete_tenant_rejects_tenant_with_children() {
    let root = Uuid::from_u128(0x100);
    let mid = Uuid::from_u128(0x110);
    let leaf = Uuid::from_u128(0x111);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );
    svc.create_tenant(&ctx_for(root), child_input(mid, root))
        .await
        .expect("mid");
    svc.create_tenant(&ctx_for(root), child_input(leaf, mid))
        .await
        .expect("leaf");

    let err = svc
        .delete_tenant(&ctx(), mid)
        .await
        .expect_err("has children");
    assert_eq!(err.code(), "tenant_has_children");
}

#[tokio::test]
async fn delete_tenant_rejects_tenant_with_rg_resources() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x200);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(CountingChecker { count: 3 }),
    );
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("child");
    let err = svc
        .delete_tenant(&ctx(), child)
        .await
        .expect_err("has resources");
    assert_eq!(err.code(), "tenant_has_resources");
}

/// `delete_tenant` is idempotent: a second call on an already-deleted
/// row returns the same tombstone (same `deleted_at`), does NOT invoke
/// the RG ownership probe, and does NOT rewrite retention metadata.
#[tokio::test]
async fn delete_tenant_is_idempotent_on_already_deleted_tenant() {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[allow(unknown_lints, de0309_must_have_domain_model)]
    struct RecordingChecker {
        calls: Arc<AtomicU64>,
    }
    #[async_trait]
    impl ResourceOwnershipChecker for RecordingChecker {
        async fn count_ownership_links(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<u64, DomainError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
    }

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x201);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let calls = Arc::new(AtomicU64::new(0));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(RecordingChecker {
            calls: calls.clone(),
        }),
    );
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("child");

    let first = svc
        .delete_tenant(&ctx(), child)
        .await
        .expect("first delete ok");
    assert_eq!(first.status, PublicTenantStatus::Deleted);
    let first_deleted_at = first.deleted_at.expect("tombstone carries deleted_at");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "first delete must probe RG exactly once"
    );

    // Snapshot the retention bookkeeping after the first delete so we
    // can prove the second call does not rewrite it.
    let retention_after_first = repo
        .state
        .lock()
        .expect("lock")
        .retention
        .get(&child)
        .copied();
    assert!(
        retention_after_first.is_some(),
        "first delete must record retention row"
    );

    // Tiny pause so a `now()` re-stamp would be visible against the
    // first call's `deleted_at`.
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let second = svc
        .delete_tenant(&ctx(), child)
        .await
        .expect("second delete must succeed (idempotent), not 409");
    assert_eq!(second.status, PublicTenantStatus::Deleted);
    assert_eq!(
        second.deleted_at,
        Some(first_deleted_at),
        "idempotent retry MUST NOT re-stamp deleted_at"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second delete MUST NOT re-probe RG"
    );
    assert_eq!(
        repo.state
            .lock()
            .expect("lock")
            .retention
            .get(&child)
            .copied(),
        retention_after_first,
        "idempotent retry MUST NOT rewrite the retention row"
    );
}

/// `suspend_tenant` / `unsuspend_tenant` on a soft-deleted tenant
/// surface as `Conflict` (the row is read-only during retention).
#[tokio::test]
async fn suspend_tenant_rejects_deleted_tenant() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x202);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("child");
    svc.delete_tenant(&ctx(), child).await.expect("soft delete");

    let err = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect_err("deleted tenant is read-only");
    assert_eq!(err.code(), "conflict");

    let err = svc
        .unsuspend_tenant(&ctx_for(root), child)
        .await
        .expect_err("deleted tenant is read-only");
    assert_eq!(err.code(), "conflict");
}

/// `suspend_tenant` on a missing tenant surfaces as `NotFound`.
#[tokio::test]
async fn suspend_tenant_returns_not_found_for_missing_tenant() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo,
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );
    let err = svc
        .suspend_tenant(&ctx_for(root), Uuid::from_u128(0xDEAD))
        .await
        .expect_err("missing tenant");
    assert_eq!(err.code(), "not_found");
}

/// `suspend_tenant` on a `Provisioning` tenant surfaces as `NotFound`
/// — the AM-internal status has no SDK representation and must not
/// leak through the lifecycle surface. Mirrors the symmetric
/// `unsuspend_tenant_rejects_provisioning_as_not_found` test.
#[tokio::test]
async fn suspend_tenant_rejects_provisioning_as_not_found() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x601);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: child,
        parent_id: Some(root),
        name: "prov".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
    let svc = make_service(repo, FakeOutcome::Ok);
    let err = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect_err("must not see provisioning tenant");
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn scan_retention_does_not_starve_due_rows_behind_older_not_due_backlog() {
    // **This test is NOT a regression test for the SQL-side
    // starvation bug.** It is a fake↔prod alignment pin. The
    // historical bug — over-fetching with `LIMIT N` ordered by
    // `scheduled_at ASC` and applying `is_due` in Rust — could
    // hide a single newer due row behind ≥256 older not-yet-due
    // NULL-window rows. The fix pushes the due-check into SQL.
    //
    // The `FakeTenantRepo` here ALREADY applies `is_due` before
    // the limit (see `test_support/repo.rs::scan_retention_due`),
    // so the assertion below pins the contract that the SQL
    // implementation *also* applies due-filter pre-limit. It does
    // not, on its own, prove the SQL implementation is correct —
    // the FakeRepo could match the contract while the SQL drifts.
    // Authoritative SQL validation lives in the integration-test
    // suite once the testcontainers scaffold lands for AM (TODO —
    // see `feature-tenant-hierarchy-management.md` retention §).
    //
    // Pathological shape exercised here: 300 older NULL-window
    // rows scheduled 80d ago (not due under default 90d retention)
    // + 1 newer row with explicit `retention_window_secs = 0`
    // (due immediately).
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));

    let now = OffsetDateTime::now_utc();
    let eighty_days_ago = now - time::Duration::days(80);
    // 90 days in seconds. `from_hours` is unstable on the
    // workspace MSRV; use the seconds form.
    #[allow(clippy::duration_suboptimal_units)]
    let ninety_day_default = std::time::Duration::from_secs(90 * 86_400);

    // 300 older NULL-window rows scheduled 80d ago -- not due under
    // the default 90d retention. 300 > the historical 4×64 = 256
    // over-fetch cap, which is what triggered the starvation.
    {
        let mut state = repo.state.lock().expect("lock");
        for i in 0..300u128 {
            let id = Uuid::from_u128(0xA000 + i);
            state.tenants.insert(
                id,
                TenantModel {
                    id,
                    parent_id: Some(root),
                    name: format!("backlog-{i}"),
                    status: TenantStatus::Deleted,
                    self_managed: false,
                    tenant_type_uuid: Uuid::from_u128(0xAA),
                    depth: 1,
                    created_at: eighty_days_ago,
                    updated_at: eighty_days_ago,
                    deleted_at: Some(eighty_days_ago),
                },
            );
            state.closure.push(ClosureRow {
                ancestor_id: id,
                descendant_id: id,
                barrier: 0,
                descendant_status: TenantStatus::Deleted.as_smallint(),
            });
            state.closure.push(ClosureRow {
                ancestor_id: root,
                descendant_id: id,
                barrier: 0,
                descendant_status: TenantStatus::Deleted.as_smallint(),
            });
            // NULL retention_window -- use service default.
            state.retention.insert(id, (eighty_days_ago, None));
        }
    }

    // The single due row: explicit retention_window_secs = 0,
    // scheduled now -> due immediately.
    let due_id = Uuid::from_u128(0xDEED);
    {
        let mut state = repo.state.lock().expect("lock");
        state.tenants.insert(
            due_id,
            TenantModel {
                id: due_id,
                parent_id: Some(root),
                name: "due-now".into(),
                status: TenantStatus::Deleted,
                self_managed: false,
                tenant_type_uuid: Uuid::from_u128(0xAA),
                depth: 1,
                created_at: now,
                updated_at: now,
                deleted_at: Some(now),
            },
        );
        state.closure.push(ClosureRow {
            ancestor_id: due_id,
            descendant_id: due_id,
            barrier: 0,
            descendant_status: TenantStatus::Deleted.as_smallint(),
        });
        state.closure.push(ClosureRow {
            ancestor_id: root,
            descendant_id: due_id,
            barrier: 0,
            descendant_status: TenantStatus::Deleted.as_smallint(),
        });
        state
            .retention
            .insert(due_id, (now, Some(std::time::Duration::from_secs(0))));
    }

    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: ninety_day_default.as_secs(),
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
        Arc::new(InertResourceOwnershipChecker),
    );

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(
        res.processed, 1,
        "exactly the due row should be processed; the 300-row not-due backlog must not starve it"
    );
    assert_eq!(res.cleaned, 1, "the due row should reach Cleaned");
    assert!(
        repo.find_by_id(&AccessScope::allow_all(), due_id)
            .await
            .expect("repo")
            .is_none(),
        "the due row must be hard-deleted"
    );
    // None of the 300 not-due rows should have been touched.
    for i in 0..300u128 {
        let id = Uuid::from_u128(0xA000 + i);
        assert!(
            repo.find_by_id(&AccessScope::allow_all(), id)
                .await
                .expect("repo")
                .is_some(),
            "not-due backlog row {id} must remain"
        );
    }
}

#[tokio::test]
async fn reaper_records_idp_not_found_as_already_absent_distinct_from_compensated() {
    // Plugin reports the vendor never had the tenant (or already
    // wiped it) — surfaces as `IdpDeprovisionFailure::NotFound`. AM
    // treats this as success-equivalent for *teardown*: the
    // provisioning row is **physically removed** (not flipped to
    // `Deleted`) because provisioning rows never become SDK-visible,
    // so retention-pipeline tombstoning would leak rows. The
    // operator-visible counter must report it under `already_absent`,
    // not `compensated`: `compensated` counts rows the reaper
    // actively cleaned, `already_absent` counts rows that were
    // already gone on the vendor side (typically a lost-claim or
    // cross-system inconsistency signal worth investigating).
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x215);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::NotFound);
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    let res = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(res.scanned, 1);
    assert_eq!(
        res.already_absent, 1,
        "NotFound must surface as already_absent, not compensated"
    );
    assert_eq!(
        res.compensated, 0,
        "compensated counts only rows we actively cleaned"
    );
    assert_eq!(res.deferred, 0, "NotFound is not a defer outcome");
    assert!(
        repo.find_by_id(&AccessScope::allow_all(), stuck)
            .await
            .expect("repo")
            .is_none(),
        "compensation must physically remove the provisioning row, not leave a tombstone"
    );
}

// `reaper_releases_claim_on_terminal_failure` was deleted: it
// asserted the old defer-on-Terminal contract (release_claim so the
// row is rescanned next tick), which is the exact loop we now reject.
// Terminal failures stamp `terminal_failure_at` instead, and the
// scan-skip invariant is covered by
// `reaper_marks_terminal_failure_and_parks_row_out_of_retry_loop`
// directly. The claim is still released for column-tidiness, but
// that is implementation detail rather than load-bearing contract.

#[tokio::test]
async fn reaper_defers_on_idp_retryable_failure() {
    // Mirror of
    // `reaper_marks_terminal_failure_and_parks_row_out_of_retry_loop`
    // for the `Retryable` arm of `reap_stuck_provisioning` —
    // Retryable defers and releases the claim (row eligible next
    // tick), unlike Terminal which stamps `terminal_failure_at`
    // and parks the row indefinitely. The Retryable path is the
    // most-likely-to-fire branch in production (transient IdP) and
    // was previously uncovered.
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x213);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Retryable);
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    let res = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(res.scanned, 1);
    assert_eq!(res.deferred, 1, "retryable failure defers");
    assert_eq!(
        res.compensated, 0,
        "retryable failure must NOT mark row compensated"
    );
    let row = repo
        .find_by_id(&AccessScope::allow_all(), stuck)
        .await
        .expect("repo")
        .expect("row");
    assert_eq!(
        row.status,
        TenantStatus::Provisioning,
        "row stays provisioning until the next reaper tick"
    );
}

#[tokio::test]
async fn reaper_redrives_on_each_tick_when_idp_keeps_failing() {
    // Reaper holds no per-tenant retry state across ticks. Retry /
    // backoff / circuit-breaker policy lives in the IdP plugin. As
    // long as the plugin keeps returning `Retryable` and the per-
    // tick claim is properly released on the defer path, every
    // tick re-issues the call — the plugin is responsible for its
    // own rate-limiting.
    //
    // This test pins that contract by running two ticks back-to-
    // back and asserting that `deprovision_tenant` was invoked
    // twice (once per tick), which only works if the claim was
    // released after the first tick's defer.
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x214);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Retryable);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let r1 = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r1.scanned, 1);
    assert_eq!(r1.deferred, 1);

    let r2 = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r2.scanned, 1, "row still present, scan picks it up");
    assert_eq!(r2.deferred, 1);

    let calls = idp.deprovision_calls.lock().expect("lock").len();
    assert_eq!(
        calls, 2,
        "stateless reaper re-issues on every tick; plugin owns rate-limiting"
    );
}

#[tokio::test]
async fn reaper_marks_terminal_failure_and_parks_row_out_of_retry_loop() {
    // Per the SDK contract, `IdpDeprovisionFailure::Terminal` is
    // non-recoverable — the IdP plugin is signalling that the
    // vendor refuses to deprovision and operator intervention is
    // required. The reaper must:
    //   * stamp `terminal_failure_at` on the row,
    //   * count the row under `result.terminal` (NOT `deferred`,
    //     which is reserved for transient defers),
    //   * NOT release the row back into the scan-eligible pool.
    //
    // The follow-up assertion exercises the second tick to pin the
    // park-out-of-loop contract: the IdP MUST NOT be re-invoked on
    // the next tick because the scan filter excludes
    // `terminal_failure_at IS NOT NULL` rows. Without this, the
    // earlier reaper would loop forever and never surface the
    // operator-action-required signal.
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x210);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Terminal);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let r1 = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r1.scanned, 1);
    assert_eq!(
        r1.terminal, 1,
        "terminal failure must surface as result.terminal, not deferred"
    );
    assert_eq!(
        r1.deferred, 0,
        "deferred is reserved for transient defers; terminal is its own bucket"
    );

    // Row stays in `Provisioning` — terminal_failure_at is the
    // operator-action-required marker, the row is not deleted nor
    // moved to a different status.
    let row = repo
        .find_by_id(&AccessScope::allow_all(), stuck)
        .await
        .expect("repo")
        .expect("row");
    assert_eq!(row.status, TenantStatus::Provisioning);

    // Direct repo-state assertion: the reason `r2.scanned == 0` below
    // MUST be the terminal-marker, not a lingering claim. Without
    // this check, a regression that left the claim in place (e.g.
    // `release_claim` no-oping or being skipped) would also yield
    // `r2.scanned == 0` because the row would be hidden behind
    // `RETENTION_CLAIM_TTL`. Asserting the terminal marker directly
    // pins the parking semantics independent of the claim path.
    assert!(
        repo.state
            .lock()
            .expect("lock")
            .terminal_failures
            .contains_key(&stuck),
        "terminal_failure_at MUST be stamped on the row (parking marker)"
    );

    // Second tick: scan filter must exclude the marked row, so the
    // IdP is not contacted again. This is the contract that the
    // pre-fix reaper violated (it would defer + release_claim and
    // re-issue indefinitely).
    let r2 = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(
        r2.scanned, 0,
        "terminal-marked row must be excluded from subsequent scans"
    );
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "IdP must NOT be re-invoked on the second tick (the marker parks the row)"
    );
}

/// Concurrent-claim invariant: a stuck `Provisioning` row that is
/// already claimed by another worker (within `RETENTION_CLAIM_TTL`)
/// MUST be skipped by `scan_stuck_provisioning`, so two replicas
/// cannot stamp duplicate `IdpPluginClient::deprovision_tenant`
/// calls onto the same row inside one TTL window.
///
/// Set-up: two stuck rows, only one of them pre-claimed via
/// `seed_claim`. The reaper tick must touch only the unclaimed row;
/// the `IdP` must see exactly one `deprovision_tenant` call; and the
/// pre-existing claim on the held row MUST remain intact (the
/// reaper's per-row release path only fires for rows it itself
/// scanned, never for rows it skipped).
#[tokio::test]
async fn reaper_skips_rows_already_claimed_by_another_worker() {
    let root = Uuid::from_u128(0x100);
    let stuck_unclaimed = Uuid::from_u128(0x217);
    let stuck_held = Uuid::from_u128(0x218);
    let other_worker = Uuid::from_u128(0xFEED_FEED);

    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    for id in [stuck_unclaimed, stuck_held] {
        repo.insert_tenant_raw(TenantModel {
            id,
            parent_id: Some(root),
            name: "stuck".into(),
            status: TenantStatus::Provisioning,
            self_managed: false,
            tenant_type_uuid: Uuid::from_u128(0xAA),
            depth: 1,
            created_at: then,
            updated_at: then,
            deleted_at: None,
        });
    }
    // Pre-seed a live claim on `stuck_held` to model "another replica
    // already claimed this row in the current TTL window."
    repo.seed_claim(stuck_held, other_worker);

    // Default `FakeDeprovisionOutcome::Ok` — this test is about
    // claim-skipping semantics, the IdP-side outcome is incidental;
    // a clean `Ok` keeps `compensated += 1` (rather than
    // `already_absent`) so the assertion below pins the
    // claim-skip invariant directly.
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let res = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(
        res.scanned, 1,
        "claimed row must be skipped; only the unclaimed row is scanned"
    );
    assert_eq!(
        res.compensated, 1,
        "the unclaimed row is the only one that gets compensated"
    );

    let calls = idp.deprovision_calls.lock().expect("lock");
    assert_eq!(
        calls.len(),
        1,
        "single IdP deprovision call -- the held row must NOT be re-issued by this replica"
    );
    assert_eq!(
        calls[0], stuck_unclaimed,
        "deprovision target is the unclaimed row"
    );
    drop(calls);

    // The pre-existing claim on `stuck_held` MUST still be there:
    // the reaper never scanned it, never released it, never touched it.
    assert!(
        repo.has_claim(stuck_held),
        "another worker's claim must not be cleared by a peer's reaper tick"
    );
    let held = repo
        .find_by_id_unchecked(stuck_held)
        .expect("held row exists");
    assert_eq!(
        held.status,
        TenantStatus::Provisioning,
        "held row must remain provisioning -- only the holder may compensate it"
    );
}

/// Concurrent-claim invariant for the retention pipeline (mirror of the
/// reaper test above). `tenants.claimed_by` backs both pipelines, so the
/// same fence MUST work end-to-end through `hard_delete_batch`: a
/// soft-deleted, retention-due row already claimed by another worker
/// MUST be skipped — no `IdpPluginClient::deprovision_tenant`
/// call, no DB teardown, the held claim survives.
///
/// Set-up: two soft-deleted children due for hard-delete; pre-seed a
/// claim on one. The tick processes only the unclaimed row.
#[tokio::test]
async fn hard_delete_batch_skips_rows_already_claimed_by_another_worker() {
    let root = Uuid::from_u128(0x100);
    let due_unclaimed = Uuid::from_u128(0x230);
    let due_held = Uuid::from_u128(0x231);
    let other_worker = Uuid::from_u128(0xFEED_BEEF);

    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let now = OffsetDateTime::now_utc();
    // Seed both as `Active` then call `schedule_deletion` to flip
    // them to `Deleted` with `retention=0` so they are immediately
    // due — `schedule_deletion` is the same call site that
    // `delete_tenant` uses, so the resulting rows + retention metadata
    // match production. (Calling it on an already-`Deleted` row is
    // rejected by the fake as `Conflict`, mirroring the real repo.)
    for id in [due_unclaimed, due_held] {
        repo.insert_tenant_raw(TenantModel {
            id,
            parent_id: Some(root),
            name: "due".into(),
            status: TenantStatus::Active,
            self_managed: false,
            tenant_type_uuid: Uuid::from_u128(0xAA),
            depth: 1,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        });
        let _ = repo
            .schedule_deletion(
                &AccessScope::allow_all(),
                id,
                now,
                Some(StdDuration::from_secs(0)),
            )
            .await
            .expect("schedule");
    }
    // Pre-seed a live claim on `due_held` to model "another replica
    // already claimed this row in the current TTL window."
    repo.seed_claim(due_held, other_worker);

    // Build the service manually rather than via `svc_with` so we
    // keep a handle on the IdP and can inspect its call list.
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(
        res.processed, 1,
        "claimed row must be skipped; only the unclaimed row is processed"
    );
    assert_eq!(
        res.cleaned, 1,
        "the unclaimed row is the only one that gets cleaned"
    );

    let calls = idp.deprovision_calls.lock().expect("lock");
    assert_eq!(
        calls.len(),
        1,
        "single IdP deprovision call -- the held row must NOT be re-issued by this replica"
    );
    assert_eq!(
        calls[0], due_unclaimed,
        "deprovision target is the unclaimed row"
    );
    drop(calls);

    // Held row's claim survives, row is still `Deleted` (not yet
    // hard-deleted) — only the holder may complete its teardown.
    assert!(
        repo.has_claim(due_held),
        "another worker's claim must not be cleared by a peer's retention tick"
    );
    let held = repo
        .find_by_id_unchecked(due_held)
        .expect("held row exists");
    assert_eq!(
        held.status,
        TenantStatus::Deleted,
        "held row must remain Deleted -- only the holder may complete its teardown"
    );
}

#[tokio::test]
async fn hard_delete_batch_skips_parent_when_child_still_exists() {
    let root = Uuid::from_u128(0x100);
    let parent = Uuid::from_u128(0x220);
    let child = Uuid::from_u128(0x221);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Direct `TenantService::new` so we keep a handle on the IdP fake
    // and can assert preflight-rejected rows never trigger
    // `deprovision_tenant` — the load-bearing contract added with
    // `check_hard_delete_eligibility`.
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    svc.create_tenant(&ctx_for(root), child_input(parent, root))
        .await
        .expect("p");
    svc.create_tenant(&ctx_for(root), child_input(child, parent))
        .await
        .expect("c");
    // Seed an already-deleted parent directly. `schedule_deletion`
    // now repeats the child guard inside its SERIALIZABLE transaction,
    // so this defensive hard-delete path is only reachable from
    // legacy/corrupt state or an older deployment.
    let now = OffsetDateTime::now_utc();
    {
        let mut state = repo.state.lock().expect("lock");
        let parent_row = state.tenants.get_mut(&parent).expect("parent row");
        parent_row.status = TenantStatus::Deleted;
        parent_row.updated_at = now;
        parent_row.deleted_at = Some(now);
        state
            .retention
            .insert(parent, (now, Some(StdDuration::from_secs(0))));
    }

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(res.processed, 1);
    assert_eq!(
        res.deferred, 1,
        "parent deferred because child still exists"
    );
    assert!(
        repo.find_by_id(&AccessScope::allow_all(), parent)
            .await
            .expect("repo")
            .is_some(),
        "parent row still present"
    );
    // Preflight contract: `check_hard_delete_eligibility` MUST short-
    // circuit BEFORE the cascade-hook + IdP step. A regression that
    // moved IdP `deprovision_tenant` ahead of the preflight (or
    // bypassed the preflight entirely) would leave external IdP-side
    // state torn down for a row AM still keeps. Pin it with a strict
    // assertion: zero IdP calls in this scenario.
    let calls = idp.deprovision_calls.lock().expect("lock");
    assert!(
        calls.is_empty(),
        "preflight MUST reject before IdP fires; observed deprovision_tenant calls: {calls:?}"
    );
}

/// Anti-starvation invariant: `DeferredChildPresent` MUST hold the
/// retention claim across the tick boundary so a backlog of blocked
/// parents cannot monopolize the next tick's `LIMIT` window before
/// shallower eligible rows. The claim ages out via
/// `RETENTION_CLAIM_TTL` (~10 min), giving a deterministic back-off
/// during which the still-undue child has either become due (and
/// will be processed leaf-first first, unblocking the parent) or
/// remains undue (and the parent waits another TTL) — without
/// starving shallower work in between.
///
/// Counterpoint: other non-cleaned outcomes (`StorageError`,
/// `NotEligible`, `IdpRetryable`) MUST clear the claim promptly so
/// the row is re-attempted on the next tick. Pinned by
/// `hard_delete_batch_skips_rows_already_claimed_by_another_worker`
/// further up the file.
#[tokio::test]
async fn hard_delete_batch_holds_claim_on_deferred_child_present() {
    let root = Uuid::from_u128(0x100);
    let parent = Uuid::from_u128(0x230);
    let child = Uuid::from_u128(0x231);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    svc.create_tenant(&ctx_for(root), child_input(parent, root))
        .await
        .expect("parent");
    svc.create_tenant(&ctx_for(root), child_input(child, parent))
        .await
        .expect("child");
    // Soft-delete the parent's status directly so the retention scan
    // picks it up while the child remains `Active` — the
    // `DeferredChildPresent` outcome path. (`schedule_deletion` itself
    // would reject this from the service entry-point because of the
    // child guard; this scenario reproduces legacy/corrupt state.)
    let now = OffsetDateTime::now_utc();
    {
        let mut state = repo.state.lock().expect("lock");
        let parent_row = state.tenants.get_mut(&parent).expect("parent row");
        parent_row.status = TenantStatus::Deleted;
        parent_row.updated_at = now;
        parent_row.deleted_at = Some(now);
        state
            .retention
            .insert(parent, (now, Some(StdDuration::from_secs(0))));
    }

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(res.processed, 1);
    assert_eq!(res.deferred, 1, "parent must be deferred (child present)");

    // The contract: claim is HELD across `DeferredChildPresent` so
    // the next tick's `scan_retention_due` does NOT re-pick this
    // parent until `RETENTION_CLAIM_TTL` ages the claim out. Without
    // this guarantee a backlog of N blocked parents would consume
    // every tick's `LIMIT` window indefinitely.
    assert!(
        repo.has_claim(parent),
        "DeferredChildPresent MUST hold the retention claim (back-off via TTL); \
         clearing it here would re-expose the parent on the very next tick and \
         starve shallower eligible rows under high blocked-parent backlog"
    );
}

/// Test-only `IdpPluginClient` that pushes `"idp"` into a
/// shared ordering log when `deprovision_tenant` is called. Paired with
/// a hook that pushes `"hook"` to the same log, this lets us prove the
/// hook-before-IdP ordering on hard-delete (rather than just counting
/// hook invocations).
#[allow(unknown_lints, de0309_must_have_domain_model)]
struct OrderingRecordingIdp {
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl account_management_sdk::IdpPluginClient for OrderingRecordingIdp {
    async fn provision_tenant(
        &self,
        _req: &account_management_sdk::IdpProvisionTenantRequest,
    ) -> Result<
        account_management_sdk::IdpProvisionResult,
        account_management_sdk::IdpProvisionFailure,
    > {
        // Not pushed: this test only verifies hook-before-IdP ordering
        // on the hard-delete path; provisioning runs before the hook
        // is even registered, so recording it would just clutter the
        // log without strengthening the assertion.
        Ok(account_management_sdk::IdpProvisionResult::default())
    }
    async fn deprovision_tenant(
        &self,
        _req: &account_management_sdk::IdpDeprovisionTenantRequest,
    ) -> Result<(), account_management_sdk::IdpDeprovisionFailure> {
        self.log.lock().expect("lock").push("idp");
        Ok(())
    }
}

#[tokio::test]
async fn hard_delete_batch_invokes_cascade_hook_before_idp() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x230);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let order_log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let idp = Arc::new(OrderingRecordingIdp {
        log: order_log.clone(),
    });
    let cfg = AccountManagementConfig {
        retention: crate::config::RetentionConfig {
            default_window_secs: 0,
            ..crate::config::RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("c");
    svc.delete_tenant(&ctx(), child).await.expect("sd");

    let log_for_hook = order_log.clone();
    let hook: TenantHardDeleteHook = Arc::new(move |_id: Uuid| {
        let log = log_for_hook.clone();
        async move {
            log.lock().expect("lock").push("hook");
            Ok::<_, HookError>(())
        }
        .boxed()
    });
    svc.register_hard_delete_hook(hook);

    let _ = svc.hard_delete_batch(64).await;

    // Ordering assertion: hook MUST observably run before IdP
    // `deprovision_tenant` so userland cascade cleanup happens while
    // the IdP-side state still exists. A counter-only test would
    // pass even if the order flipped.
    let entries = order_log.lock().expect("lock").clone();
    assert_eq!(
        entries,
        vec!["hook", "idp"],
        "hook must run before IdP deprovision_tenant; observed: {entries:?}"
    );
}

/// Regression for the panic-hook → infinite-`Retryable`-loop hazard
/// AND for the retention-side `terminal_failure_at` parking that
/// stops the row from churning the scanner.
///
/// A cascade hook whose returned future panics on poll is spawned
/// into its own task by the retention pipeline, so the panic does
/// not kill the loop. The pipeline maps the resulting `JoinError`
/// to `HookError::Terminal` (NOT `HookError::Retryable`) — so the
/// row's outcome is `CascadeTerminal` (folded into `failed`), not
/// `CascadeRetryable` (folded into `deferred`). After classifying
/// the row as terminal, the pipeline calls
/// `mark_retention_terminal_failure` so the row drops out of the
/// scanner via the `terminal_failure_at IS NULL` filter — without
/// this, a permanently buggy hook would keep re-failing every
/// `tick_secs` forever with no progress and pure observability
/// noise.
///
/// Operator runbook: clear `terminal_failure_at` (manual SQL)
/// after fixing the hook impl; the next scan picks the row back
/// up. Pinned by the second-tick / cleared-marker / third-tick
/// sequence below.
#[tokio::test]
async fn hard_delete_batch_panicking_cascade_hook_classifies_terminal_and_parks() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x240);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let cfg = AccountManagementConfig {
        retention: crate::config::RetentionConfig {
            default_window_secs: 0,
            ..crate::config::RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child");
    svc.delete_tenant(&ctx(), child).await.expect("soft delete");

    // Hook returns a future that panics on first poll. The pipeline
    // spawns it via `tokio::spawn`, so the panic surfaces as
    // `JoinError::is_panic()` rather than killing the loop.
    let hook: TenantHardDeleteHook = Arc::new(|_id: Uuid| {
        async move {
            panic!("simulated cascade hook bug");
            #[allow(unreachable_code)]
            Ok::<(), HookError>(())
        }
        .boxed()
    });
    svc.register_hard_delete_hook(hook);

    // Tick 1 — panic classifies as CascadeTerminal and the row is
    // parked.
    let res = svc.hard_delete_batch(64).await;

    assert_eq!(res.processed, 1);
    assert_eq!(
        res.failed, 1,
        "panic must classify as CascadeTerminal (failed), \
         not CascadeRetryable (deferred)"
    );
    assert_eq!(
        res.deferred, 0,
        "regression to the old Retryable mapping would inflate `deferred`"
    );
    assert_eq!(res.cleaned, 0);
    assert!(
        idp.deprovision_calls.lock().expect("lock").is_empty(),
        "IdP deprovision must NOT be called when a cascade hook panics"
    );
    assert!(
        repo.find_by_id_unchecked(child).is_some(),
        "tenant row must remain in place after CascadeTerminal"
    );
    assert!(
        repo.is_terminally_failed_unchecked(child),
        "row must be parked via terminal_failure_at after CascadeTerminal"
    );
    // Claim must be released after parking — otherwise an operator
    // who clears `terminal_failure_at` would still need to wait
    // `RETENTION_CLAIM_TTL` (~10 min) before the scanner re-claims
    // the row. A regression that gates `release_claim_now` on
    // `!is_failed()` (vs the current `!is_cleaned()` logic) would
    // break the `CascadeTerminal` and `IdpTerminal` paths but
    // would still release on `StorageError` (which `is_failed()`
    // also covers); the same regression would silently leave a
    // live claim on `NotEligible` and `IdpRetryable` outcomes,
    // which this test does not exercise. A future
    // `IdpRetryable`-focused parking test should include the
    // parallel `find_claim_unchecked(...).is_none()` assertion to
    // close that coverage gap.
    assert!(
        repo.find_claim_unchecked(child).is_none(),
        "claim must be released after CascadeTerminal parking"
    );

    // Tick 2 — parked row is invisible to the scanner. Without
    // parking, the broken hook would re-fail every tick forever
    // (the regression class this assertion fences against).
    let res2 = svc.hard_delete_batch(64).await;
    assert_eq!(
        res2.processed, 0,
        "parked row must drop out of scan_retention_due"
    );
    assert_eq!(res2.failed, 0);
    assert_eq!(res2.deferred, 0);
    assert_eq!(res2.cleaned, 0);

    // Operator runbook: ship a fixed hook + redeploy, then clear
    // `terminal_failure_at` so the next tick picks the row back
    // up. Modeled here as a fresh `TenantService` over the SAME
    // repo + idp state — equivalent to a process restart that
    // drains the in-memory hook list, after which sibling features
    // re-register their (fixed) hooks at init. The fake repo state
    // already has the row in `Deleted + parked + retention-due`
    // shape, so we only need a service with a healthy hook list to
    // drive the post-fix tick.
    //
    // `clear_terminal_failure_unchecked` simulates the manual SQL
    // UPDATE; it also clears any leftover claim entry because the
    // fake repo has no TTL-based takeover (production releases the
    // claim at parking time, so on the prod side this is a no-op).
    let svc_after_fix = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    let healthy_hook: TenantHardDeleteHook =
        Arc::new(|_id: Uuid| async move { Ok::<(), HookError>(()) }.boxed());
    svc_after_fix.register_hard_delete_hook(healthy_hook);

    repo.clear_terminal_failure_unchecked(child);
    assert!(
        !repo.is_terminally_failed_unchecked(child),
        "operator-cleared row must no longer carry terminal_failure_at"
    );

    // Tick 3 — cleared row re-enters the scanner and completes.
    // This pins the runbook end-to-end: identify the broken row,
    // ship a healthy hook, clear the marker, the next tick hard-
    // deletes. Without this assertion the "scanner picks the row
    // back up" half of the runbook is only stated, not proven.
    let res3 = svc_after_fix.hard_delete_batch(64).await;
    assert_eq!(
        res3.processed, 1,
        "operator-cleared row must re-enter scan_retention_due"
    );
    assert_eq!(
        res3.cleaned, 1,
        "healthy hook + IdP `Ok` should reach Cleaned on tick 3"
    );
    assert!(
        repo.find_by_id_unchecked(child).is_none(),
        "tenant row must be hard-deleted after operator-driven recovery"
    );
}

/// Pins the parking path for the IdP-terminal arm of the retention
/// pipeline. Symmetric to the cascade-hook parking test above:
/// when the `IdP` returns `IdpDeprovisionFailure::Terminal` during
/// `hard_delete_batch`, the row is classified as `IdpTerminal` and
/// must be parked via `mark_retention_terminal_failure` so the
/// scanner stops re-attempting on every tick. Mirrors the reaper's
/// posture for `Provisioning` rows the `IdP` classifies as terminal.
#[tokio::test]
async fn hard_delete_batch_idp_terminal_parks_row_via_terminal_failure_at() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x250);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Terminal);
    let cfg = AccountManagementConfig {
        retention: crate::config::RetentionConfig {
            default_window_secs: 0,
            ..crate::config::RetentionConfig::default()
        },
        ..AccountManagementConfig::default()
    };
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child");
    svc.delete_tenant(&ctx(), child).await.expect("soft delete");

    // Tick 1 — IdP returns Terminal → IdpTerminal outcome → row
    // parked.
    let res = svc.hard_delete_batch(64).await;
    assert_eq!(res.processed, 1);
    assert_eq!(res.failed, 1);
    assert!(
        repo.is_terminally_failed_unchecked(child),
        "row must be parked via terminal_failure_at after IdpTerminal"
    );
    // Claim must be released after parking — see the rationale on
    // the symmetric assertion in the cascade-hook parking test
    // above.
    assert!(
        repo.find_claim_unchecked(child).is_none(),
        "claim must be released after IdpTerminal parking"
    );

    // Tick 2 — parked row is invisible to the scanner.
    let res2 = svc.hard_delete_batch(64).await;
    assert_eq!(
        res2.processed, 0,
        "parked row must drop out of scan_retention_due on subsequent ticks"
    );
}

#[tokio::test]
async fn check_hierarchy_integrity_returns_one_entry_per_category_in_fixed_order() {
    use crate::domain::tenant::integrity::IntegrityCategory;

    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service(repo, FakeOutcome::Ok);

    let report = svc
        .check_hierarchy_integrity()
        .await
        .expect("clean fake repo must yield a violation-free report");

    let categories: Vec<IntegrityCategory> = report
        .violations_by_category
        .iter()
        .map(|(cat, _)| *cat)
        .collect();
    assert_eq!(
        categories,
        IntegrityCategory::all().to_vec(),
        "report categories must follow IntegrityCategory::all() order"
    );
    assert_eq!(report.total(), 0, "trivial fake repo has no violations");
    for (_, viols) in &report.violations_by_category {
        assert!(viols.is_empty(), "every category must be zero-valued");
    }
}

#[tokio::test]
async fn check_hierarchy_integrity_buckets_violations_into_fixed_categories() {
    // Repo returns a flat `Vec<(category, violation)>` in arbitrary
    // order; the service rebuckets it into the fixed
    // `IntegrityCategory::all()` order with empty Vec entries for
    // categories the repo did not surface. The flat shape gives the
    // service room to deduplicate / merge if it ever wants to —
    // pinning the rebucketing here protects that contract.
    use crate::domain::tenant::integrity::{IntegrityCategory, Violation};

    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.set_audit_violations(vec![
        // Two violations on Cycle (third category in the fixed order).
        (
            IntegrityCategory::Cycle,
            Violation {
                category: IntegrityCategory::Cycle,
                tenant_id: Some(Uuid::from_u128(0x200)),
                details: "cycle a->b->a".into(),
            },
        ),
        (
            IntegrityCategory::Cycle,
            Violation {
                category: IntegrityCategory::Cycle,
                tenant_id: Some(Uuid::from_u128(0x201)),
                details: "cycle b->c->b".into(),
            },
        ),
        // One on the LAST category — observable proof of fixed-order
        // preservation regardless of repo emission order.
        (
            IntegrityCategory::DescendantStatusDivergence,
            Violation {
                category: IntegrityCategory::DescendantStatusDivergence,
                tenant_id: Some(Uuid::from_u128(0x202)),
                details: "stale descendant_status".into(),
            },
        ),
    ]);
    let svc = make_service(repo, FakeOutcome::Ok);

    let report = svc
        .check_hierarchy_integrity()
        .await
        .expect("audit returning violations must still produce a report");

    let categories: Vec<IntegrityCategory> = report
        .violations_by_category
        .iter()
        .map(|(cat, _)| *cat)
        .collect();
    assert_eq!(
        categories,
        IntegrityCategory::all().to_vec(),
        "rebucketing must preserve IntegrityCategory::all() order"
    );

    let counts: Vec<(IntegrityCategory, usize)> = report
        .violations_by_category
        .iter()
        .map(|(cat, v)| (*cat, v.len()))
        .collect();
    assert_eq!(
        counts
            .iter()
            .copied()
            .find(|(c, _)| *c == IntegrityCategory::Cycle)
            .map(|(_, n)| n),
        Some(2),
        "Cycle bucket must have both violations"
    );
    assert_eq!(
        counts
            .iter()
            .copied()
            .find(|(c, _)| *c == IntegrityCategory::DescendantStatusDivergence)
            .map(|(_, n)| n),
        Some(1),
        "DescendantStatusDivergence bucket must have its single violation"
    );
    // Every other category — empty.
    for (cat, n) in &counts {
        if !matches!(
            cat,
            IntegrityCategory::Cycle | IntegrityCategory::DescendantStatusDivergence
        ) {
            assert_eq!(*n, 0, "category {cat:?} must be empty in this scenario");
        }
    }
    assert_eq!(report.total(), 3);
}

#[tokio::test]
async fn check_hierarchy_integrity_propagates_repo_error() {
    // If the repo refuses the audit (e.g. another worker holds the
    // single-flight gate, surfacing as `IntegrityCheckInProgress`) the
    // service `?`-propagates the error untouched. The metric / log
    // emission paths are bypassed because no report exists to
    // categorise — pinning this is what tells callers
    // `IntegrityCheckInProgress` is observable on the public surface.

    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.set_audit_error(DomainError::IntegrityCheckInProgress);
    let svc = make_service(repo, FakeOutcome::Ok);

    let err = svc
        .check_hierarchy_integrity()
        .await
        .expect_err("repo error must propagate");

    assert!(
        matches!(err, DomainError::IntegrityCheckInProgress),
        "expected IntegrityCheckInProgress, got {err:?}"
    );
}

#[tokio::test]
async fn repair_hierarchy_integrity_rejects_when_disabled() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Default config has `integrity_check.repair.enabled = false` —
    // the on-demand entry MUST surface `FeatureDisabled` and
    // MUST NOT mutate `tenant_closure`.
    let closure_before = repo.snapshot_closure();
    let svc = make_service(repo.clone(), FakeOutcome::Ok);

    let err = svc
        .repair_hierarchy_integrity()
        .await
        .expect_err("repair must be rejected when disabled");
    assert!(
        matches!(err, DomainError::FeatureDisabled { ref detail } if detail.contains("disabled")),
        "expected FeatureDisabled with 'disabled' detail, got {err:?}"
    );

    // Defence-in-depth: assert closure is bit-for-bit unchanged.
    // A future regression that called the repo *before* the gate
    // check would still leave the error visible here, but the
    // closure mutation would slip past — this assertion closes
    // that gap.
    let closure_after = repo.snapshot_closure();
    assert_eq!(
        closure_before, closure_after,
        "disabled repair must NOT mutate tenant_closure"
    );
}

#[tokio::test]
async fn repair_hierarchy_integrity_clean_repo_returns_empty_report() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = make_service_repair_enabled(repo, FakeOutcome::Ok);

    let report = svc
        .repair_hierarchy_integrity()
        .await
        .expect("clean repair must succeed");

    assert_eq!(report.total_repaired(), 0);
    assert_eq!(report.total_deferred(), 0);
    assert_eq!(
        report.repaired_per_category.len(),
        5,
        "all 5 derivable categories present"
    );
    assert_eq!(
        report.deferred_per_category.len(),
        5,
        "all 5 deferred categories present"
    );
    for (_, count) in &report.repaired_per_category {
        assert_eq!(*count, 0);
    }
    for (_, count) in &report.deferred_per_category {
        assert_eq!(*count, 0);
    }
}

#[tokio::test]
async fn repair_hierarchy_integrity_buckets_derivable_vs_deferred() {
    use crate::domain::tenant::integrity::{IntegrityCategory, Violation};

    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // 2× MissingClosureSelfRow (derivable) + 1× OrphanedChild
    // (operator triage). Use `set_repair_violations` because the
    // fake exposes a dedicated repair-side script slot independent
    // of the check-side `next_audit_outcome` (see
    // `FakeTenantRepo::repair_derivable_closure_violations` doc).
    repo.set_repair_violations(vec![
        (
            IntegrityCategory::MissingClosureSelfRow,
            Violation {
                category: IntegrityCategory::MissingClosureSelfRow,
                tenant_id: Some(Uuid::from_u128(0xA1)),
                details: String::new(),
            },
        ),
        (
            IntegrityCategory::MissingClosureSelfRow,
            Violation {
                category: IntegrityCategory::MissingClosureSelfRow,
                tenant_id: Some(Uuid::from_u128(0xA2)),
                details: String::new(),
            },
        ),
        (
            IntegrityCategory::OrphanedChild,
            Violation {
                category: IntegrityCategory::OrphanedChild,
                tenant_id: Some(Uuid::from_u128(0xB1)),
                details: String::new(),
            },
        ),
    ]);
    let svc = make_service_repair_enabled(repo, FakeOutcome::Ok);

    let report = svc
        .repair_hierarchy_integrity()
        .await
        .expect("repair succeeds");

    let missing_self_row = report
        .repaired_per_category
        .iter()
        .find(|(c, _)| *c == IntegrityCategory::MissingClosureSelfRow)
        .map(|(_, n)| *n)
        .expect("missing-self-row entry present");
    assert_eq!(
        missing_self_row, 2,
        "two derivable violations bucketed as repaired"
    );

    let orphan = report
        .deferred_per_category
        .iter()
        .find(|(c, _)| *c == IntegrityCategory::OrphanedChild)
        .map(|(_, n)| *n)
        .expect("orphan entry present");
    assert_eq!(orphan, 1, "non-derivable violation bucketed as deferred");
}

#[tokio::test]
async fn repair_hierarchy_integrity_propagates_repo_error() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.set_repair_error(DomainError::IntegrityCheckInProgress);
    let svc = make_service_repair_enabled(repo, FakeOutcome::Ok);

    let err = svc
        .repair_hierarchy_integrity()
        .await
        .expect_err("gate-conflict error must propagate");

    assert!(
        matches!(err, DomainError::IntegrityCheckInProgress),
        "expected IntegrityCheckInProgress, got {err:?}"
    );
}

#[tokio::test]
async fn hard_delete_concurrency_processes_siblings_in_parallel() {
    // Five sibling leaves at the same depth, processed under
    // `hard_delete_concurrency = 4`. We pin parallelism via an
    // observable in-flight counter rather than wall-clock — a
    // sequential regression would peak at `1`, while any genuine
    // parallelism peaks at `>= 2`. The cap is `concurrency` (=4),
    // so peak ∈ {2, 3, 4} on a healthy `buffer_unordered`
    // implementation. This avoids the CI-flakiness that a wall-
    // clock assertion (e.g. `elapsed < 200ms`) introduces on
    // shared / debug-build runners where scheduling adds 30–40%
    // overhead.
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                hard_delete_concurrency: 4,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
        Arc::new(InertResourceOwnershipChecker),
    );

    for i in 0..5u128 {
        let id = Uuid::from_u128(0x300 + i);
        svc.create_tenant(&ctx_for(root), child_input(id, root))
            .await
            .expect("child");
        svc.delete_tenant(&ctx(), id).await.expect("sd");
    }

    let in_flight = Arc::new(AtomicU32::new(0));
    let peak = Arc::new(AtomicU32::new(0));
    let hits = Arc::new(AtomicU32::new(0));
    let in_flight_for_hook = in_flight.clone();
    let peak_for_hook = peak.clone();
    let hits_for_hook = hits.clone();
    let hook: TenantHardDeleteHook = Arc::new(move |_id: Uuid| {
        let inf = in_flight_for_hook.clone();
        let pk = peak_for_hook.clone();
        let hc = hits_for_hook.clone();
        async move {
            // Increment in-flight FIRST, then update peak — order
            // matters: peak is the running maximum of concurrent
            // hooks observed by any single hook.
            let cur = inf.fetch_add(1, Ordering::SeqCst) + 1;
            pk.fetch_max(cur, Ordering::SeqCst);
            // Hold long enough that all sibling hooks dispatched
            // by `buffer_unordered` are simultaneously in-flight.
            // The actual duration is irrelevant to the assertion;
            // it just creates a window in which siblings overlap.
            tokio::time::sleep(StdDuration::from_millis(50)).await;
            inf.fetch_sub(1, Ordering::SeqCst);
            hc.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HookError>(())
        }
        .boxed()
    });
    svc.register_hard_delete_hook(hook);

    let res = svc.hard_delete_batch(64).await;

    assert_eq!(res.processed, 5);
    assert_eq!(res.cleaned, 5, "all five leaves should reach Cleaned");
    assert_eq!(hits.load(Ordering::SeqCst), 5);
    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak >= 2,
        "expected parallel processing (peak >= 2); got peak = {observed_peak} \
         (sequential single-flight would peak at 1)"
    );
    assert!(
        observed_peak <= 4,
        "peak in-flight {observed_peak} exceeds hard_delete_concurrency = 4 \
         — buffer_unordered cap appears broken"
    );
}

#[tokio::test]
async fn strict_mode_rejects_deep_child() {
    // Per `algo-depth-threshold-evaluation` strict-mode rejects at
    // `depth > threshold`. Build a chain of depth 0..=2 and pin
    // `depth_threshold = 2` so a child created under the deepest
    // tenant lands at depth 3 (= threshold + 1) and is rejected.
    let repo = Arc::new(FakeTenantRepo::new());
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    let mut prev: Option<Uuid> = None;
    let mut deepest = Uuid::nil();
    for i in 0..=2u128 {
        let id = Uuid::from_u128(0x2000 + i);
        repo.insert_tenant_raw(TenantModel {
            id,
            parent_id: prev,
            name: format!("t{i}"),
            status: TenantStatus::Active,
            self_managed: false,
            tenant_type_uuid: Uuid::from_u128(0xAA),
            depth: u32::try_from(i).expect("u32"),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        });
        prev = Some(id);
        deepest = id;
    }

    let cfg = AccountManagementConfig {
        hierarchy: crate::config::HierarchyConfig {
            depth_strict_mode: true,
            depth_threshold: 2,
        },
        ..AccountManagementConfig::default()
    };
    let svc = svc_with(
        repo,
        FakeOutcome::Ok,
        cfg,
        Arc::new(InertResourceOwnershipChecker),
    );
    let child = Uuid::from_u128(0x9001);
    let root = Uuid::from_u128(0x2000);
    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, deepest))
        .await
        .expect_err("strict reject");
    assert_eq!(err.code(), "tenant_depth_exceeded");
}

// =================================================================
// FEATURE 2.3 -- tenant-type-enforcement (saga step 3)
// =================================================================

/// Programmable [`TenantTypeChecker`] used by the saga step 3 tests
/// to drive the type-compatibility barrier through its three
/// branches: admit, type-not-allowed reject, registry unavailable.
#[allow(unknown_lints, de0309_must_have_domain_model)]
struct FakeTenantTypeChecker {
    outcome: Mutex<FakeTypeOutcome>,
    calls: Mutex<Vec<(Uuid, Uuid)>>,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
enum FakeTypeOutcome {
    Admit,
    TypeNotAllowed { detail: &'static str },
    ServiceUnavailable { detail: &'static str },
}

impl FakeTenantTypeChecker {
    fn new(outcome: FakeTypeOutcome) -> Self {
        Self {
            outcome: Mutex::new(outcome),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(Uuid, Uuid)> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait]
impl TenantTypeChecker for FakeTenantTypeChecker {
    async fn check_parent_child(
        &self,
        parent_type: Uuid,
        child_type: Uuid,
    ) -> Result<(), DomainError> {
        self.calls
            .lock()
            .expect("lock")
            .push((parent_type, child_type));
        match self.outcome.lock().expect("lock").clone() {
            FakeTypeOutcome::Admit => Ok(()),
            FakeTypeOutcome::TypeNotAllowed { detail } => Err(DomainError::TypeNotAllowed {
                detail: detail.into(),
            }),
            FakeTypeOutcome::ServiceUnavailable { detail } => {
                Err(DomainError::ServiceUnavailable {
                    detail: detail.into(),
                    retry_after: None,
                    cause: None,
                })
            }
        }
    }
}

fn make_service_with_type_checker(
    repo: Arc<FakeTenantRepo>,
    outcome: FakeOutcome,
    type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
) -> TenantService<FakeTenantRepo> {
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(outcome)),
        Arc::new(InertResourceOwnershipChecker),
        type_checker,
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(Arc::new(ConstantTypesRegistry))
}

/// AC §6 first bullet -- when the parent's `tenant_type` is not in
/// the child's `allowed_parent_types`, the barrier rejects with
/// `type_not_allowed` and no `tenants` row is written.
#[tokio::test]
async fn create_tenant_rejects_when_parent_type_not_in_child_allowed_parents() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x500);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let closure_before = repo.snapshot_closure().len();
    let svc = make_service_with_type_checker(
        repo.clone(),
        FakeOutcome::Ok,
        Arc::new(FakeTenantTypeChecker::new(
            FakeTypeOutcome::TypeNotAllowed {
                detail: "customer not allowed under platform",
            },
        )),
    );

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("type-not-allowed must reject");
    assert_eq!(err.code(), "type_not_allowed");
    assert_eq!(err.http_status(), 400);

    // No `tenants` row, no closure rows written.
    let row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(row.is_none(), "no tenant row should be written on reject");
    assert_eq!(repo.snapshot_closure().len(), closure_before);
}

/// Barrier admits -> saga proceeds and the checker observed exactly
/// one `(parent_type, child_type)` call with the right shape.
#[tokio::test]
async fn create_tenant_succeeds_when_parent_child_compatible() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let checker = Arc::new(FakeTenantTypeChecker::new(FakeTypeOutcome::Admit));
    let svc = make_service_with_type_checker(repo, FakeOutcome::Ok, checker.clone());

    // Root tenant_type_uuid is `0xAA` per `FakeTenantRepo::with_root`,
    // and the child uuid is derived from the chained-id string in
    // `child_input` via `gts::GtsID::new(...).to_uuid()`.
    let created = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("compatible types admit");
    assert_eq!(created.id.0, child);
    assert_eq!(created.status, PublicTenantStatus::Active);

    let expected_child_type_uuid =
        gts::GtsID::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")
            .expect("valid gts chain")
            .to_uuid();
    let calls = checker.calls();
    assert_eq!(calls.len(), 1, "barrier must be invoked exactly once");
    assert_eq!(calls[0].0, Uuid::from_u128(0xAA), "parent type");
    assert_eq!(calls[0].1, expected_child_type_uuid, "child type");
}

/// AC §6 fifth bullet -- when GTS is unreachable, the saga propagates
/// `service_unavailable` (HTTP 503) and writes nothing.
#[tokio::test]
async fn create_tenant_propagates_types_registry_unavailable_as_service_unavailable() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x502);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let closure_before = repo.snapshot_closure().len();
    let svc = make_service_with_type_checker(
        repo.clone(),
        FakeOutcome::Ok,
        Arc::new(FakeTenantTypeChecker::new(
            FakeTypeOutcome::ServiceUnavailable {
                detail: "types-registry: connection refused",
            },
        )),
    );

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("registry down must propagate");
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);

    // No DB side effects.
    let row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(row.is_none(), "no tenant row on registry unavailable");
    assert_eq!(repo.snapshot_closure().len(), closure_before);
}

/// AC §6 third bullet, negative half -- same-type nesting requested
/// but the GTS schema does not include the type in its own
/// `allowed_parent_types`. Drive via the checker stub returning
/// `type_not_allowed` for the same-type pairing.
#[tokio::test]
async fn create_tenant_rejects_same_type_nesting_when_disallowed() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x503);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Align the root's `tenant_type_uuid` with the chain-derived UUID
    // produced by `child_input(...).tenant_type` so the checker is
    // actually invoked with `parent_type == child_type` (true same-type
    // pairing). Without this alignment the test would exercise the
    // mixed-type path instead — see assertions at `*_calls_checker_*`
    // for the chain UUID derivation.
    let same_type_uuid = gts::GtsID::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")
        .expect("valid gts chain")
        .to_uuid();
    repo.state
        .lock()
        .expect("lock")
        .tenants
        .get_mut(&root)
        .expect("root seeded by with_root")
        .tenant_type_uuid = same_type_uuid;
    let closure_before = repo.snapshot_closure().len();
    let checker = Arc::new(FakeTenantTypeChecker::new(
        FakeTypeOutcome::TypeNotAllowed {
            detail: "type cannot nest under itself",
        },
    ));
    let svc = make_service_with_type_checker(repo.clone(), FakeOutcome::Ok, checker.clone());

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("disallowed same-type nesting must reject");
    assert_eq!(err.code(), "type_not_allowed");
    assert_eq!(err.http_status(), 400);

    // Confirm the checker really saw a same-type pairing (this is the
    // load-bearing part of the test name).
    let calls = checker.calls();
    assert_eq!(calls.len(), 1, "barrier must be invoked exactly once");
    assert_eq!(
        calls[0].0, calls[0].1,
        "parent_type and child_type must match for the same-type branch"
    );
    assert_eq!(calls[0].0, same_type_uuid);

    let row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(row.is_none());
    assert_eq!(repo.snapshot_closure().len(), closure_before);
}

/// AC §6 third bullet, positive half -- same-type nesting requested
/// and the GTS schema admits the type as its own allowed parent.
#[tokio::test]
async fn create_tenant_accepts_same_type_nesting_when_allowed() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x504);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Align root tenant_type_uuid with the chain-derived child UUID —
    // see sibling test for the rationale.
    let same_type_uuid = gts::GtsID::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")
        .expect("valid gts chain")
        .to_uuid();
    repo.state
        .lock()
        .expect("lock")
        .tenants
        .get_mut(&root)
        .expect("root seeded by with_root")
        .tenant_type_uuid = same_type_uuid;
    let checker = Arc::new(FakeTenantTypeChecker::new(FakeTypeOutcome::Admit));
    let svc = make_service_with_type_checker(repo.clone(), FakeOutcome::Ok, checker.clone());

    let created = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("same-type nesting admitted by checker must succeed");
    assert_eq!(created.id.0, child);
    assert_eq!(created.status, PublicTenantStatus::Active);

    let calls = checker.calls();
    assert_eq!(calls.len(), 1, "barrier must be invoked exactly once");
    assert_eq!(
        calls[0].0, calls[0].1,
        "parent_type and child_type must match for the same-type branch"
    );
    assert_eq!(calls[0].0, same_type_uuid);
}

// =================================================================
// Contract-review test gaps (F1–F4)
//
// The Phase-1/2/3 contract review identified four acceptance
// criteria whose implementing code was already in place but lacked
// dedicated assertions. The tests below close those gaps using the
// same in-memory `FakeTenantRepo` + `FakeIdpProvisioner` machinery
// as the rest of the module.
// =================================================================

/// F1 -- `Suspended -> Deleted` soft-delete transition.
///
/// `service::delete_tenant` admits any SDK-visible non-Deleted source
/// status, but the existing happy-path test only covered an active
/// leaf. This test moves the leaf through suspension first via
/// `suspend_tenant` and then asserts the soft-delete still flips
/// the row to `Deleted` with retention metadata.
#[tokio::test]
async fn delete_tenant_succeeds_on_suspended_leaf_tenant() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child");

    // Move the leaf to Suspended via the dedicated lifecycle method.
    let suspended = svc
        .suspend_tenant(&ctx_for(root), child)
        .await
        .expect("suspension allowed");
    assert_eq!(suspended.status, PublicTenantStatus::Suspended);

    // Now soft-delete the suspended leaf -- should succeed.
    let deleted = svc
        .delete_tenant(&ctx_for(root), child)
        .await
        .expect("soft-delete suspended leaf");
    assert_eq!(deleted.status, PublicTenantStatus::Deleted);
    // Retention bookkeeping must be present after soft-delete.
    assert!(
        repo.state
            .lock()
            .expect("lock")
            .retention
            .contains_key(&child),
        "retention row must be written for the soft-deleted tenant"
    );
}

/// Pin the public-contract requirement that soft-delete stamps
/// `tenants.deleted_at`. The `OpenAPI` `Tenant.deleted_at` field is
/// surfaced on every tenant response, the migration declares a
/// partial index `idx_tenants_deleted_at` keyed on this column, the
/// retention-scan index (`idx_tenants_retention_scan`) keys on it as
/// well, and the `Tenant` schema lists it as the public-contract
/// tombstone marker. Missing this stamp would empty both partial
/// indexes AND surface soft-deleted rows with
/// `status=deleted, deleted_at=null` to the API.
#[tokio::test]
async fn delete_tenant_stamps_deleted_at_on_returned_model_and_subsequent_reads() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF101);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = svc_with(
        repo.clone(),
        FakeOutcome::Ok,
        AccountManagementConfig::default(),
        Arc::new(InertResourceOwnershipChecker),
    );

    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child");
    let row_after_create = repo
        .find_by_id_unchecked(child)
        .expect("freshly-created row in fake repo");
    assert!(
        row_after_create.deleted_at.is_none(),
        "freshly created tenant must not carry a deleted_at timestamp"
    );

    let deleted = svc
        .delete_tenant(&ctx_for(root), child)
        .await
        .expect("soft-delete leaf");
    assert_eq!(deleted.status, PublicTenantStatus::Deleted);
    // `deleted_at` MUST be populated on the public `Tenant` surface
    // after soft-delete — admin UIs read it to render the "Will be
    // removed on …" banner (deadline = `deleted_at + retention_window`)
    // without a follow-up retention round-trip. `None` would mean the
    // lifter dropped the field or `schedule_deletion` never stamped
    // the column.
    let public_deleted_at = deleted
        .deleted_at
        .expect("Tenant.deleted_at must be Some after soft-delete");

    // The `schedule_deletion` contract is asserted via the storage
    // row directly through the unchecked accessor that bypasses the
    // SDK-visibility filter (deleted rows are filtered out by
    // `get_tenant`).
    let after = repo
        .find_by_id_unchecked(child)
        .expect("row still present pre hard-delete");
    let stamped = after
        .deleted_at
        .expect("schedule_deletion must stamp deleted_at");
    assert_eq!(
        stamped, after.updated_at,
        "deleted_at and updated_at are written in the same transaction \
         and should match `now` exactly"
    );
    assert_eq!(
        public_deleted_at, stamped,
        "Tenant.deleted_at must equal the persisted storage value \
         (same-tx stamp)"
    );
}

/// F2 -- `hard_delete_batch` row-level outcome on
/// `IdpDeprovisionFailure::Terminal`.
///
/// The existing `reaper_marks_terminal_failure_and_parks_row_out_of_retry_loop`
/// test covers the reaper path. This adds the missing assertion for
/// the hard-delete batch path: a soft-deleted tenant whose `IdP`
/// deprovision returns `Terminal` is tagged `IdpTerminal` (counted
/// as a failed/deferred outcome by `HardDeleteResult::tally`) and
/// the `tenants` row is NOT reclaimed.
#[tokio::test]
async fn hard_delete_batch_marks_idp_terminal_failure_as_failed() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let tenant = repo.seed_soft_deleted_child_due_for_hard_delete(root);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Terminal);
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(res.processed, 1, "exactly one due row was processed");
    assert_eq!(
        res.failed, 1,
        "IdP terminal failure must count exactly once toward `failed`, got {res:?}"
    );
    assert_eq!(
        res.cleaned, 0,
        "row must NOT be reclaimed on IdP terminal failure"
    );
    // Tenant row + closure rows still in the DB -- the reaper /
    // operator owns the next move, not the hard-delete batch.
    assert!(
        repo.find_by_id_unchecked(tenant).is_some(),
        "soft-deleted row must remain after IdP terminal"
    );
    // Positive control vs `hard_delete_batch_holds_claim_on_deferred_child_present`:
    // `IdpTerminal` is a non-cleaned, non-`DeferredChildPresent` outcome,
    // so the retention claim MUST be released so the next tick can
    // re-attempt classification (peer reaper or this worker may pick
    // up the row again). Without this counterpoint, a regression that
    // held the claim for ALL non-cleaned outcomes would silently
    // delay every retryable / terminal row by `RETENTION_CLAIM_TTL`.
    assert!(
        !repo.has_claim(tenant),
        "non-DeferredChildPresent failure outcomes MUST release the claim promptly \
         so the row is re-attempted on the next tick"
    );
}

/// F3 -- finalization-TX failure injection (saga step 3 abort).
///
/// Saga full-compensation contract: once `idp.provision_tenant`
/// has succeeded, any saga step-3 failure
/// (`load_ancestor_chain` / `activate_tenant`) MUST trigger
/// best-effort `deprovision_tenant` + row compensation **inside
/// the saga** so vendor-side state is not orphaned until the next
/// reaper tick. The original error from step 3 still propagates;
/// the reaper remains the last-resort cleanup if either
/// compensation step fails (e.g. a peer reaper claimed the row
/// mid-activation, or the `IdP` returned `Retryable` / `Terminal`).
#[tokio::test]
async fn create_tenant_finalization_tx_failure_compensates_idp_and_row() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF300);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.expect_next_activation_failure("simulated SERIALIZABLE abort");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "activate_tenant failure must surface as Internal, got {result:?}"
    );
    // Saga-side compensation MUST have invoked the IdP's
    // `deprovision_tenant` exactly once for the failed
    // provisioning row — otherwise vendor-side state is orphaned
    // until the next reaper tick.
    let deprovision_calls = idp.deprovision_calls.lock().expect("lock").clone();
    assert_eq!(
        deprovision_calls.as_slice(),
        &[child],
        "saga MUST best-effort deprovision the IdP after step-3 failure; got {deprovision_calls:?}"
    );
    // Row delete is best-effort and runs only after the IdP
    // confirms cleanup. With `FakeIdpProvisioner::Ok` the
    // happy-path compensation succeeds end-to-end and the row is
    // gone — no reaper hop needed for this scenario.
    let provisioning_rows = repo.snapshot_provisioning_rows();
    assert!(
        provisioning_rows.is_empty(),
        "saga MUST best-effort delete the provisioning row after IdP cleanup; \
         got {provisioning_rows:?}"
    );
}

/// Saga-side compensation MUST be best-effort: when the `IdP`
/// returns `Retryable` (vendor-side retry needed), the saga MUST
/// NOT delete the local provisioning row — the reaper owns that
/// retry. Pins the contract that compensation degrades to the
/// reaper instead of orphaning vendor-side state.
#[tokio::test]
async fn create_tenant_finalization_failure_with_retryable_idp_leaves_row_for_reaper() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF301);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.expect_next_activation_failure("simulated SERIALIZABLE abort");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    // Provision OK (saga step 2), but compensation deprovision
    // returns Retryable (vendor-side retry needed). Saga must NOT
    // delete the row in this case — leave it for the reaper.
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Retryable);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "activate_tenant failure must surface as Internal, got {result:?}"
    );
    // IdP compensation was attempted exactly once.
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "saga MUST attempt best-effort IdP compensation"
    );
    // Row MUST remain — the IdP's `Retryable` signals vendor-side
    // state still exists; deleting locally would orphan it. The
    // reaper owns the retry from here.
    let provisioning_rows = repo.snapshot_provisioning_rows();
    assert_eq!(
        provisioning_rows.len(),
        1,
        "provisioning row MUST be left for the reaper when IdP compensation is non-clean; \
         got {provisioning_rows:?}"
    );
    assert_eq!(provisioning_rows[0].id, child);
}

/// Pins the `upsert_idp_metadata` up-front persistence: when the
/// saga's step-3 finalization fails AND the best-effort
/// compensation cannot confirm vendor-side teardown, the plugin's
/// per-tenant metadata blob MUST already be in `tenant_idp_metadata`
/// so the reaper can rebuild `IdpDeprovisionTenantRequest` carrying the
/// blob on the next tick. Closes codex deep-review P1#2: without the
/// up-front upsert, the only copy of `provision_result.metadata`
/// dies in the saga's stack frame and the reaper forwards an empty
/// `TenantContext::metadata`, silently leaking vendor-side state.
#[tokio::test]
async fn create_tenant_finalization_failure_persists_idp_metadata_for_reaper() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF303);
    let plugin_blob = serde_json::json!({"realm": "acme-prod", "vendor_token": "opaque-blob"});

    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.expect_next_activation_failure("simulated SERIALIZABLE abort");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    // Plugin returns non-empty metadata on provision so the reaper
    // recovery path is load-bearing.
    idp.set_metadata(Some(plugin_blob.clone()));
    // Non-clean compensation outcome forces the saga to leave the
    // row + metadata for the reaper.
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Retryable);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "activate_tenant failure must surface as Internal, got {result:?}"
    );

    // Provisioning row MUST stay (Retryable compensation leaves it
    // for the reaper).
    let provisioning_rows = repo.snapshot_provisioning_rows();
    assert_eq!(provisioning_rows.len(), 1, "row left for reaper");
    assert_eq!(provisioning_rows[0].id, child);

    // The load-bearing assertion: `tenant_idp_metadata` carries the
    // plugin blob, so the reaper can rebuild `IdpDeprovisionTenantRequest`
    // with the plugin's per-tenant state. A regression that drops
    // the up-front `upsert_idp_metadata` call would leave this map
    // empty for `child` and silently orphan vendor-side resources
    // on the next reaper tick.
    let metadata = repo
        .find_idp_metadata(&AccessScope::allow_all(), child)
        .await
        .expect("find_idp_metadata succeeds");
    assert_eq!(
        metadata.as_ref(),
        Some(&plugin_blob),
        "tenant_idp_metadata MUST carry the plugin blob so the reaper can deprovision with it; got {metadata:?}"
    );
}

/// Closes codex review P1: `upsert_idp_metadata` failure between
/// `provision_tenant` and `finalize_provisioning` MUST run the
/// best-effort `compensate_failed_activation` rung, NOT exit
/// silently with `?`. A transient DB blip on the metadata write
/// would otherwise leave the `IdP` holding vendor-side state with
/// no AM-side handle to deprovision it later — exactly the bug
/// pattern codex flagged on the previous review pass.
#[tokio::test]
async fn create_tenant_upsert_idp_metadata_failure_runs_idp_compensation() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF304);

    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Arm the failure injection on the pre-activation metadata
    // upsert. `expect_next_activation_failure` is NOT armed — we
    // want the failure to surface BEFORE `finalize_provisioning`
    // runs.
    repo.expect_next_upsert_idp_metadata_failure("simulated transient DB blip");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_metadata(Some(serde_json::json!({"realm": "acme-prod"})));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "upsert_idp_metadata failure must surface as Internal, got {result:?}"
    );

    // The load-bearing assertion: IdP compensation MUST have been
    // attempted exactly once. A regression that drops the
    // compensation rung on this path (the previous `?` shape)
    // would leave `deprovision_calls` empty here and the vendor-
    // side tenant orphaned.
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "saga MUST attempt IdP compensation when upsert_idp_metadata fails after provision_tenant succeeded"
    );
}

/// Closes codex review P2: explicit `tenant_idp_metadata` DELETE
/// in `compensate_provisioning`. With the pre-activation upsert
/// landing the row BEFORE the `Provisioning → Active` flip, a
/// clean saga compensation would otherwise leave an orphaned
/// metadata row on `SQLite` (no enforced FK). Pins that the row is
/// gone after the compensation path succeeds (`FakeTenantRepo`
/// mirrors the production explicit DELETE).
#[tokio::test]
async fn create_tenant_clean_compensation_removes_pre_activation_idp_metadata() {
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF305);
    let plugin_blob = serde_json::json!({"realm": "acme-prod"});

    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.expect_next_activation_failure("simulated SERIALIZABLE abort");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_metadata(Some(plugin_blob.clone()));
    // Clean compensation outcome — `Ok(())` means the saga proves
    // vendor-side cleanup ran and proceeds to local row delete,
    // which MUST also remove the metadata row.
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Ok);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    // Activation was armed to fail; surface is `Internal`. The
    // post-error assertions below are the load-bearing checks.
    assert!(matches!(result, Err(DomainError::Internal { .. })));

    // Provisioning row removed by clean compensation.
    let provisioning_rows = repo.snapshot_provisioning_rows();
    assert!(
        provisioning_rows.is_empty(),
        "clean compensation MUST delete the provisioning row; got {provisioning_rows:?}"
    );
    // The load-bearing assertion: metadata row MUST also be gone.
    // On Postgres production this is FK CASCADE; on SQLite the
    // `compensate_provisioning` explicit DELETE is the only thing
    // preventing the leak.
    let metadata = repo
        .find_idp_metadata(&AccessScope::allow_all(), child)
        .await
        .expect("find_idp_metadata succeeds");
    assert!(
        metadata.is_none(),
        "tenant_idp_metadata row MUST be cleaned up by compensate_provisioning; got {metadata:?}"
    );
}

/// Saga step-3 compensation must apply the same `idp.required`
/// gate as the retention pipeline and the reaper: a real plugin
/// returning `IdpDeprovisionFailure::UnsupportedOperation` under
/// `idp.required = true` signals that vendor-side state may still
/// exist and the AM row MUST be left for the reaper, NOT
/// hard-deleted locally. Without this gate the saga compensation
/// would orphan vendor-side state every time a real plugin lacks
/// the `deprovision_tenant` impl — the same bug class already
/// fenced for retention by
/// `hard_delete_batch_defers_unsupported_when_idp_required_true`
/// and for the reaper by
/// `reaper_marks_unsupported_terminal_when_idp_required_true`.
#[tokio::test]
async fn create_tenant_finalization_failure_with_unsupported_idp_required_leaves_row_for_reaper() {
    use crate::config::IdpConfig;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xF302);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    repo.expect_next_activation_failure("simulated SERIALIZABLE abort");
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    // Provision OK (saga step 2). On the saga step-3 compensation
    // path the IdP returns `UnsupportedOperation` — under
    // `idp.required = true` this MUST be treated as "did not
    // confirm cleanup" (defer to reaper), NOT as the
    // NoopIdpProvider-style "no IdP-side state retained".
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Unsupported);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            idp: IdpConfig { required: true },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let result = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await;
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "activate_tenant failure must surface as Internal, got {result:?}"
    );
    // IdP compensation was attempted exactly once — the saga
    // ALWAYS calls `deprovision_tenant` on step-3 failure to give
    // the plugin a chance to confirm cleanup.
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "saga MUST attempt best-effort IdP compensation"
    );
    // Row MUST remain — under `idp.required = true`,
    // `UnsupportedOperation` cannot prove vendor-side state is
    // gone, so deleting locally would orphan it. The reaper owns
    // the cleanup from here. A regression that drops this gate
    // (treating `UnsupportedOperation` as `idp_clean = true`
    // unconditionally) would leave `provisioning_rows` empty
    // here.
    let provisioning_rows = repo.snapshot_provisioning_rows();
    assert_eq!(
        provisioning_rows.len(),
        1,
        "provisioning row MUST be left for the reaper when IdP returns UnsupportedOperation \
         under idp.required=true; got {provisioning_rows:?}"
    );
    assert_eq!(provisioning_rows[0].id, child);
}

// ---------------------------------------------------------------------------
// `cfg.idp.required` gates `UnsupportedOperation` semantics
// ---------------------------------------------------------------------------
//
// `IdpDeprovisionFailure::UnsupportedOperation` is only safe to map
// to "skip IdP, continue local teardown" when the deployment
// opted out of an IdP entirely (`idp.required = false` → wired to
// `NoopIdpProvider`). When a real plugin returns this variant,
// vendor-side state may still exist and the AM row MUST NOT be
// removed locally — the retention pipeline defers (`IdpRetryable`)
// and the reaper parks (`Terminal`, operator action required).

#[tokio::test]
async fn hard_delete_batch_defers_unsupported_when_idp_required_true() {
    use crate::config::IdpConfig;

    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let tenant = repo.seed_soft_deleted_child_due_for_hard_delete(root);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Unsupported);
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            idp: IdpConfig { required: true },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let res = svc.hard_delete_batch(64).await;
    assert_eq!(res.processed, 1);
    assert_eq!(
        res.cleaned, 0,
        "row MUST NOT be cleaned when IdP returns UnsupportedOperation under idp.required=true \
         (would orphan vendor-side state)"
    );
    assert_eq!(
        res.deferred, 1,
        "UnsupportedOperation under idp.required=true MUST classify as IdpRetryable (deferred); \
         got {res:?}"
    );
    assert!(
        repo.find_by_id_unchecked(tenant).is_some(),
        "row MUST remain after UnsupportedOperation under idp.required=true"
    );
}

#[tokio::test]
async fn reaper_marks_unsupported_terminal_when_idp_required_true() {
    use crate::config::IdpConfig;

    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x340);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    idp.set_deprovision_outcome(FakeDeprovisionOutcome::Unsupported);
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            idp: IdpConfig { required: true },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let r = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r.scanned, 1);
    assert_eq!(
        r.compensated, 0,
        "reaper MUST NOT compensate (hard-delete) the row on UnsupportedOperation when \
         idp.required=true - would orphan vendor-side state"
    );
    assert_eq!(
        r.terminal, 1,
        "UnsupportedOperation under idp.required=true MUST be parked terminal (operator action \
         required)"
    );
    assert!(
        repo.find_by_id_unchecked(stuck).is_some(),
        "row MUST remain after UnsupportedOperation under idp.required=true"
    );
    assert!(
        repo.state
            .lock()
            .expect("lock")
            .terminal_failures
            .contains_key(&stuck),
        "terminal_failure_at MUST be stamped"
    );
}

// ---------------------------------------------------------------------------
// Production-checker timeout boundary — service-level integration
// ---------------------------------------------------------------------------
//
// Each external integration (`RgResourceOwnershipChecker` /
// `GtsTenantTypeChecker`) has a unit test in its own `infra/*/checker.rs`
// that exercises `tokio::time::timeout`. The two tests below close the
// integration loop: they wire the **production** checker (with a tight
// 10 ms timeout) into a real `TenantService` and trigger the timeout
// via a slow SDK fake under `#[tokio::test(start_paused = true)]`,
// proving the `DomainError::ServiceUnavailable` propagates through the
// service layer with no DB side-effects.

#[tokio::test(start_paused = true)]
async fn delete_tenant_propagates_rg_timeout_as_service_unavailable() {
    use crate::infra::rg::RgResourceOwnershipChecker;
    use crate::infra::rg::test_helpers::SlowRgClient;
    use std::sync::Arc as StdArc;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x911);
    let repo = Arc::new(FakeTenantRepo::with_root(root));

    // Wire the production RG checker around a fake whose
    // `list_groups` sleeps for 50 ms; checker timeout is 10 ms.
    let slow = StdArc::new(SlowRgClient::new(StdDuration::from_millis(50)));
    let checker = Arc::new(RgResourceOwnershipChecker::with_timeout(slow, 10));

    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        checker,
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    // Create a child to act on.
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create");

    let err = svc
        .delete_tenant(&ctx_for(root), child)
        .await
        .expect_err("RG timeout must surface as service_unavailable");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);

    // Service-side invariant: timeout MUST NOT have flipped the row.
    let row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo")
        .expect("row");
    assert_eq!(
        row.status,
        TenantStatus::Active,
        "tenant must remain Active when the RG probe times out"
    );
}

#[tokio::test(start_paused = true)]
async fn create_tenant_propagates_gts_timeout_as_service_unavailable() {
    use crate::infra::types_registry::GtsTenantTypeChecker;
    use crate::infra::types_registry::test_helpers::SlowRegistry;
    use std::sync::Arc as StdArc;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x912);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let closure_before = repo.snapshot_closure().len();

    // Wire the production GTS checker around a registry whose
    // `get_type_schemas_by_uuid` sleeps for 50 ms; checker
    // timeout is 10 ms.
    let slow = StdArc::new(SlowRegistry::new(StdDuration::from_millis(50)));
    let checker = Arc::new(GtsTenantTypeChecker::with_timeout(slow, 10));

    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        checker,
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("GTS timeout must surface as service_unavailable");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);

    // Service-side invariant: a barrier-time fault MUST NOT have
    // written the child or any closure rows for it.
    let row = repo
        .find_by_id(&AccessScope::allow_all(), child)
        .await
        .expect("repo");
    assert!(row.is_none(), "no tenant row on GTS-timeout reject");
    assert_eq!(
        repo.snapshot_closure().len(),
        closure_before,
        "no closure rows on GTS-timeout reject"
    );
}

// ---------------------------------------------------------------------------
// Constraint-bearing PDP scope — `tenants`-entity subtree-clamp contract
// ---------------------------------------------------------------------------
//
// The `tenants` entity declares `resource_col = "id"` (see
// `entity/tenants.rs`) so the
// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
// predicate (cyberware-rust#1813) compiles into
// `tenants.id IN (SELECT descendant_id FROM tenant_closure
//   WHERE ancestor_id = :root AND barrier = 0)` at the secure-
// extension layer. The service forwards the compiled `AccessScope`
// from the PDP gate verbatim into the repo so authorization runs
// defence-in-depth: the gate authorizes the operation, the SQL JOIN
// clamps the row. The tests below pin both sides of the contract:
//
// * Positive — a caller whose PDP-narrowed scope **includes** the
//   target's tenant in the subtree must still see / update / delete
//   the row. The pre-#1813 contract was inverted (`scope` MUST be
//   discarded to avoid `WHERE false`); the post-#1813 contract is
//   "scope flows in, subtree-clamp lets the descendant through".
// * Negative — a caller whose PDP-narrowed scope is rooted at a
//   different ancestor (target NOT in the subtree) collapses to
//   `NotFound` at the database. This is the cross-tenant-denial
//   guarantee the previous `allow_all`-passing posture could never
//   express.

#[tokio::test]
async fn get_tenant_clamps_to_subtree_under_constraint_bearing_pdp() {
    // Wires the constraint-bearing PDP fake rooted at `root`. The
    // compiled scope is `InTenantSubtree(RESOURCE_ID, root)`; the
    // `FakeTenantRepo` walks its closure (root self-row + the
    // `(root, child)` ancestor row stamped by `create_tenant`'s
    // saga) to materialise `subtree(root) = {root, child}`. The
    // child is in the subtree so the read succeeds.
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        constraint_bearing_enforcer(root),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    svc.create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child via mock provisioning happy path");

    let info = svc
        .get_tenant(&ctx_for(root), child)
        .await
        .expect("authorized read of a descendant inside the caller's subtree MUST succeed");
    assert_eq!(info.id.0, child);
}

#[tokio::test]
async fn update_tenant_clamps_to_subtree_under_constraint_bearing_pdp() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        constraint_bearing_enforcer(root),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    let target = Uuid::from_u128(0x301);
    svc.create_tenant(&ctx_for(root), child_input(target, root))
        .await
        .expect("create child via mock provisioning happy path");

    // Subtree clamp at the secure-extension layer must let an UPDATE
    // on a descendant inside the caller's subtree through. The
    // patched name proves the write actually landed (a scope
    // mishandling that turned the SELECT-fence into a silent
    // mismatch would short-circuit before the UPDATE).
    let patch = account_management_sdk::UpdateTenantRequest::new().with_name("renamed");
    let updated = svc
        .update_tenant(&ctx_for(root), target, patch)
        .await
        .expect("authorized update of a descendant inside the caller's subtree MUST succeed");
    assert_eq!(updated.name, "renamed");
}

#[tokio::test]
async fn delete_tenant_clamps_to_subtree_under_constraint_bearing_pdp() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        constraint_bearing_enforcer(root),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));
    let target = Uuid::from_u128(0x401);
    svc.create_tenant(&ctx_for(root), child_input(target, root))
        .await
        .expect("create child via mock provisioning happy path");

    let deleted = svc.delete_tenant(&ctx_for(root), target).await.expect(
        "authorized delete_tenant of a descendant inside the caller's subtree MUST succeed",
    );
    assert_eq!(
        deleted.status,
        account_management_sdk::TenantStatus::Deleted
    );
}

#[tokio::test]
async fn get_tenant_outside_caller_subtree_returns_not_found() {
    // Cross-subtree denial: build a tree with root + child, then
    // wire a `ConstraintBearingAuthZResolver` rooted at `child`
    // (i.e. the caller's PDP only permits subtree(child) = {child}).
    // Reading `root` through this service MUST collapse to
    // `NotFound` — the secure-extension layer's subtree clamp
    // (`tenants.id IN (SELECT descendant_id FROM tenant_closure
    //   WHERE ancestor_id = child)`) leaves `root` out of the
    // descendant set. Pre-#1813 this case would have returned the
    // `root` row because the repo was called with `allow_all`.
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    // Setup uses `mock_enforcer` so `create_tenant` operates inside
    // the caller's own subtree (`subject_tenant_id = root`,
    // subtree(root) = {root, child}). After the saga, we hand the
    // repo to a fresh service wired with
    // `constraint_bearing_enforcer(child)` to model a caller
    // authorised to a strictly narrower subtree than the operation
    // it is about to attempt.
    let setup_svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    );
    setup_svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child via mock provisioning happy path");

    let svc = TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        constraint_bearing_enforcer(child),
        AccountManagementConfig::default(),
    );

    let err = svc
        .get_tenant(&ctx_for(root), root)
        .await
        .expect_err("cross-subtree read MUST collapse to NotFound at the secure-extension layer");
    match err {
        DomainError::NotFound { .. } => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Helper: build a 2-tenant tree (root + child) via the saga, then
/// return a fresh service wired with `constraint_bearing_enforcer(child)`
/// — i.e. a caller authorised to a strictly narrower subtree
/// (`subtree(child) = {child}`) than the tenant they are about to
/// touch. Used by the cross-subtree denial regression tests below.
async fn make_cross_subtree_svc(
    root: Uuid,
    child: Uuid,
) -> (TenantService<FakeTenantRepo>, Arc<FakeTenantRepo>) {
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let setup_svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    );
    setup_svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect("create child via mock provisioning happy path");

    let svc = TenantService::new(
        repo.clone(),
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        constraint_bearing_enforcer(child),
        AccountManagementConfig::default(),
    );
    (svc, repo)
}

#[tokio::test]
async fn update_tenant_outside_caller_subtree_returns_not_found() {
    // Symmetric to `get_tenant_outside_caller_subtree_returns_not_found`:
    // an UPDATE on a tenant outside the caller's subtree MUST collapse
    // to `NotFound` at the database layer. The find_by_id pre-update
    // load runs under the narrowed scope and the SELECT-fence in
    // `update_tenant_mutable`'s SERIALIZABLE retry also subtree-clamps
    // — either is sufficient to defeat the write.
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let (svc, _repo) = make_cross_subtree_svc(root, child).await;

    let patch = account_management_sdk::UpdateTenantRequest::new().with_name("renamed");
    let err = svc
        .update_tenant(&ctx_for(root), root, patch)
        .await
        .expect_err("cross-subtree update MUST collapse to NotFound");
    match err {
        DomainError::NotFound { .. } => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tenant_outside_caller_subtree_returns_not_found() {
    // Symmetric to update: soft-delete on a tenant outside the
    // caller's subtree must NOT succeed. The find_by_id disclosure
    // read is gated by the subtree clamp; the schedule_deletion
    // write would also fence on its own SELECT-then-UPDATE if the
    // read leaked through.
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let (svc, _repo) = make_cross_subtree_svc(root, child).await;

    let err = svc
        .delete_tenant(&ctx_for(root), root)
        .await
        .expect_err("cross-subtree delete_tenant MUST collapse to NotFound");
    match err {
        DomainError::NotFound { .. } => {}
        // `RootTenantCannotDelete` would mask the scope-clamp signal
        // -- surface it as a regression if it ever leaks here,
        // because it would mean the find_by_id read returned the
        // root row despite the narrowed scope.
        other => panic!(
            "expected NotFound from scope-clamped find_by_id, got {other:?} (scope clamp leaked?)"
        ),
    }
}

#[tokio::test]
async fn list_children_outside_caller_subtree_returns_not_found() {
    // The parent-existence guard inside `list_children` resolves
    // `parent_id` under the caller's narrowed scope. When the caller
    // is scoped to `subtree(child)` and the parent argument is
    // `root`, the find_by_id returns None → NotFound. Without the
    // scope plumbing, this would silently return the listing as an
    // empty page (or `root`'s children list), leaking topology.
    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0x501);
    let (svc, _repo) = make_cross_subtree_svc(root, child).await;

    let query = ODataQuery::default().with_limit(10);
    let err = svc
        .list_children(&ctx_for(root), root, &query)
        .await
        .expect_err("cross-subtree list_children MUST collapse to NotFound at the parent gate");
    match err {
        DomainError::NotFound { .. } => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Reaper terminal-counter / metric emission timing
// ---------------------------------------------------------------------------

/// Test-only `IdP` that returns `IdpDeprovisionFailure::Terminal` AND, as
/// a side effect of the call, steals the row's claim by overwriting
/// `state.claims[id]` with a sentinel "peer worker" UUID. This
/// reproduces the lost-claim race: the reaper's `mark_provisioning_terminal_failure`
/// runs on a row whose claim has rotated to a peer between
/// `classify_deprovision` and the mark UPDATE, so the fake's claim
/// fence rejects the mark and returns `Ok(false)`. Pins:
///   * `result.terminal` is bumped only on confirmed `Ok(true)`
///   * The colocated `am.tenant_retention{outcome=terminal}` metric
///     is emitted only on confirmed `Ok(true)` (the M3 fix moved it
///     out of `classify_deprovision` to prevent inflating the dashboard
///     counter relative to actually-stamped rows)
struct ClaimStealingTerminalIdp {
    repo: Arc<FakeTenantRepo>,
    peer_worker: Uuid,
}

#[async_trait]
impl account_management_sdk::IdpPluginClient for ClaimStealingTerminalIdp {
    async fn provision_tenant(
        &self,
        _req: &account_management_sdk::IdpProvisionTenantRequest,
    ) -> Result<
        account_management_sdk::IdpProvisionResult,
        account_management_sdk::IdpProvisionFailure,
    > {
        Ok(account_management_sdk::IdpProvisionResult::default())
    }
    async fn deprovision_tenant(
        &self,
        req: &account_management_sdk::IdpDeprovisionTenantRequest,
    ) -> Result<(), account_management_sdk::IdpDeprovisionFailure> {
        // Mid-call: rotate the claim to a peer worker. Production
        // analogue: the original claim's `RETENTION_CLAIM_TTL`
        // elapsed during the IdP round-trip and a peer reaper claimed
        // the row.
        self.repo
            .seed_claim(req.tenant_context.tenant_id, self.peer_worker);
        Err(account_management_sdk::IdpDeprovisionFailure::Terminal {
            detail: "vendor refuses".into(),
        })
    }
}

#[tokio::test]
async fn reaper_terminal_counter_does_not_bump_on_lost_claim_during_mark() {
    // M3 contract: `result.terminal` and the colocated
    // `am.tenant_retention{outcome=terminal}` metric MUST only fire
    // on a confirmed `Ok(true)` from
    // `mark_provisioning_terminal_failure`. The earlier code emitted
    // the metric inside `classify_deprovision` — that path inflates
    // the dashboard counter over rows whose `terminal_failure_at` was
    // never actually stamped (lost claim, storage fault). The result
    // counter and the metric live in the same lexical block, so this
    // assertion transitively pins the metric-emission timing too.
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x310);
    let peer = Uuid::from_u128(0xBEEF);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(ClaimStealingTerminalIdp {
        repo: repo.clone(),
        peer_worker: peer,
    });
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let r = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r.scanned, 1);
    assert_eq!(
        r.terminal, 0,
        "terminal counter MUST NOT bump when mark_provisioning_terminal_failure \
         returns Ok(false) (lost claim); the metric is colocated with this counter, \
         so a regression here means the dashboard's `outcome=terminal` count would \
         drift above actually-stamped rows"
    );
    assert_eq!(
        r.deferred, 1,
        "lost-claim-on-mark must be counted as `deferred` so the row is re-attempted \
         on the next tick"
    );
    // The terminal marker MUST NOT be stamped — the fake's claim
    // fence rejected the mark UPDATE.
    assert!(
        !repo
            .state
            .lock()
            .expect("lock")
            .terminal_failures
            .contains_key(&stuck),
        "terminal_failure_at MUST NOT be set when the claim fence rejected the mark"
    );
}

/// Test-only `IdP` that returns `IdpDeprovisionFailure::NotFound`
/// (success-equivalent -> drives the reaper's `Compensable` path)
/// AND, as a side effect of the call, rotates the row's claim to a
/// peer worker AND stamps `terminal_failure_at`. Reproduces the
/// race where worker A's `deprovision_tenant` exceeded
/// `RETENTION_CLAIM_TTL`, peer B re-claimed and parked the row
/// terminal, then A returns and would (pre-fix) erase B's
/// terminal-park via `compensate_provisioning`.
struct ClaimStealingCompensableIdp {
    repo: Arc<FakeTenantRepo>,
    peer_worker: Uuid,
}

#[async_trait]
impl account_management_sdk::IdpPluginClient for ClaimStealingCompensableIdp {
    async fn provision_tenant(
        &self,
        _req: &account_management_sdk::IdpProvisionTenantRequest,
    ) -> Result<
        account_management_sdk::IdpProvisionResult,
        account_management_sdk::IdpProvisionFailure,
    > {
        Ok(account_management_sdk::IdpProvisionResult::default())
    }
    async fn deprovision_tenant(
        &self,
        req: &account_management_sdk::IdpDeprovisionTenantRequest,
    ) -> Result<(), account_management_sdk::IdpDeprovisionFailure> {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_500).expect("epoch");
        // Mid-call: peer worker takes over and parks the row terminal.
        // Production analogue: `RETENTION_CLAIM_TTL` elapsed during the
        // IdP round-trip, peer reaper's scan picked the row up,
        // peer's IdP classified `Terminal`, peer stamped
        // `terminal_failure_at`. The original worker now returns
        // from its own (success-equivalent) IdP call.
        {
            let mut state = self.repo.state.lock().expect("lock");
            state
                .claims
                .insert(req.tenant_context.tenant_id, self.peer_worker);
            state
                .terminal_failures
                .insert(req.tenant_context.tenant_id, now);
        }
        Err(account_management_sdk::IdpDeprovisionFailure::NotFound {
            detail: "vendor reports already absent".into(),
        })
    }
}

#[tokio::test]
async fn reaper_compensable_path_refuses_delete_when_peer_reclaimed_and_parked_terminal() {
    // Claim-fence contract on `compensate_provisioning`: the
    // delete MUST fence on `claimed_by` (and on
    // `terminal_failure_at IS NULL`) so a worker whose
    // `RETENTION_CLAIM_TTL` elapsed during a long IdP round-trip
    // does not silently erase a peer worker's terminal-park work.
    // Without the fence, this worker's `Compensable` outcome would
    // delete the row, dropping the operator-action-required signal
    // the peer reaper just stamped.
    let root = Uuid::from_u128(0x100);
    let stuck = Uuid::from_u128(0x320);
    let peer = Uuid::from_u128(0xDEAD);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let then = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: stuck,
        parent_id: Some(root),
        name: "stuck".into(),
        status: TenantStatus::Provisioning,
        self_managed: false,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth: 1,
        created_at: then,
        updated_at: then,
        deleted_at: None,
    });
    let idp = Arc::new(ClaimStealingCompensableIdp {
        repo: repo.clone(),
        peer_worker: peer,
    });
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let r = svc.reap_stuck_provisioning(StdDuration::from_secs(0)).await;
    assert_eq!(r.scanned, 1);
    assert_eq!(
        r.compensated, 0,
        "compensable counter MUST NOT bump when the row was re-claimed and parked \
         terminal mid-call - a successful compensate_provisioning here would erase \
         the peer reaper's operator-action-required signal"
    );
    assert_eq!(
        r.already_absent, 0,
        "already_absent counter MUST NOT bump under the peer-reclaim race"
    );
    assert_eq!(
        r.deferred, 1,
        "compensate_failed must be counted as `deferred` so the row is observable \
         on the dashboard while operator action remains required"
    );
    // The row MUST still be present, parked terminal under the
    // peer's claim. (Production: the row is left for operator
    // intervention, not silently dropped.)
    assert!(
        repo.find_by_id_unchecked(stuck).is_some(),
        "row MUST still be present after refused compensation"
    );
    let state = repo.state.lock().expect("lock");
    assert!(
        state.terminal_failures.contains_key(&stuck),
        "peer reaper's terminal_failure_at marker MUST be preserved"
    );
    assert_eq!(
        state.claims.get(&stuck).copied(),
        Some(peer),
        "peer's claim on the row MUST be preserved (this worker's release_claim is a no-op against the wrong owner)"
    );
}

// ---------------------------------------------------------------------------
// Vendor-detail redaction in the hard-delete retention pipeline
// ---------------------------------------------------------------------------
//
// Per `domain/idp/redact_provider_detail`, vendor SDK detail strings
// can carry hostnames, endpoint paths, or token-bearing fragments.
// The `am.retention` log target has long retention, so the raw text
// MUST be redacted into (FNV-1a digest, char length) before it
// reaches a tracing event. The reaper companion in
// `service/reaper.rs` already does this; the tests below pin the
// same redaction contract on the **hard-delete batch** path, which
// is the call site flagged by independent reviewers (Codex P1 +
// subagent B1).

/// Test-only `IdP` that returns `IdpDeprovisionFailure::{Retryable,Terminal}`
/// with operator-supplied detail strings, so the redaction test can
/// feed in unmistakable sentinel substrings and assert they do NOT
/// appear in any captured tracing event.
#[allow(unknown_lints, de0309_must_have_domain_model)]
struct LeakySentinelIdp {
    detail: &'static str,
    classify_terminal: bool,
}

#[async_trait]
impl account_management_sdk::IdpPluginClient for LeakySentinelIdp {
    async fn provision_tenant(
        &self,
        _req: &account_management_sdk::IdpProvisionTenantRequest,
    ) -> Result<
        account_management_sdk::IdpProvisionResult,
        account_management_sdk::IdpProvisionFailure,
    > {
        Ok(account_management_sdk::IdpProvisionResult::default())
    }
    async fn deprovision_tenant(
        &self,
        _req: &account_management_sdk::IdpDeprovisionTenantRequest,
    ) -> Result<(), account_management_sdk::IdpDeprovisionFailure> {
        if self.classify_terminal {
            Err(account_management_sdk::IdpDeprovisionFailure::Terminal {
                detail: self.detail.into(),
            })
        } else {
            Err(account_management_sdk::IdpDeprovisionFailure::Retryable {
                detail: self.detail.into(),
            })
        }
    }
}

/// Sentinel that combines two redaction-relevant shapes the
/// `redact_provider_detail` doc calls out: a token-bearing
/// fragment AND an internal hostname. Both must be absent from
/// the captured log buffer if redaction is correctly applied.
const VENDOR_SECRET_SENTINEL: &str = "TOKEN-LEAK-9f3a7c2e host=secret.internal";

#[tokio::test]
#[tracing_test::traced_test]
async fn hard_delete_batch_redacts_vendor_detail_on_retryable_failure() {
    // The `am.retention` warn! event for `IdpDeprovisionFailure::Retryable`
    // MUST log only the FNV-1a digest + character length of the
    // provider detail, not the raw string. Pinned via `tracing-test`'s
    // global capture; the assertions are negative (sentinel ABSENT)
    // plus positive (digest field PRESENT).
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let _tenant = repo.seed_soft_deleted_child_due_for_hard_delete(root);
    let idp = Arc::new(LeakySentinelIdp {
        detail: VENDOR_SECRET_SENTINEL,
        classify_terminal: false,
    });
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let _res = svc.hard_delete_batch(64).await;

    // Negative — raw sentinel substrings MUST NOT appear anywhere
    // in the captured tracing buffer (would mean the warn! event
    // smuggled the unredacted detail into a structured field).
    assert!(
        !logs_contain("TOKEN-LEAK-9f3a7c2e"),
        "raw vendor token leaked into tracing buffer (am.retention redaction broken)"
    );
    assert!(
        !logs_contain("secret.internal"),
        "internal hostname leaked into tracing buffer (am.retention redaction broken)"
    );

    // Positive — the warn! event MUST emit the redacted shape.
    // `provider_detail_digest` and `provider_detail_len` are the
    // canonical field names from `domain/idp::redact_provider_detail`.
    assert!(
        logs_contain("provider_detail_digest"),
        "expected redacted-digest field on retention warn event; \
         counterpart contract pinned by `redact_provider_detail` doc"
    );
    assert!(
        logs_contain("provider_detail_len"),
        "expected redacted-length field on retention warn event"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn hard_delete_batch_redacts_vendor_detail_on_terminal_failure() {
    // Sibling assertion to the Retryable case: the `Terminal` arm
    // is the higher-blast-radius path (operator action required, the
    // detail often quotes vendor stack traces) so the redaction
    // must hold here too.
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let _tenant = repo.seed_soft_deleted_child_due_for_hard_delete(root);
    let idp = Arc::new(LeakySentinelIdp {
        detail: VENDOR_SECRET_SENTINEL,
        classify_terminal: true,
    });
    let svc = TenantService::new(
        repo.clone(),
        idp,
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: crate::config::RetentionConfig {
                default_window_secs: 0,
                ..crate::config::RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let _res = svc.hard_delete_batch(64).await;

    assert!(
        !logs_contain("TOKEN-LEAK-9f3a7c2e"),
        "raw vendor token leaked on Terminal arm (am.retention redaction broken)"
    );
    assert!(
        !logs_contain("secret.internal"),
        "internal hostname leaked on Terminal arm (am.retention redaction broken)"
    );
    assert!(
        logs_contain("provider_detail_digest"),
        "expected redacted-digest field on Terminal-arm retention warn event"
    );
    assert!(
        logs_contain("provider_detail_len"),
        "expected redacted-length field on Terminal-arm retention warn event"
    );
}

// ---- catalog-drift on load_tenant_context surfaces as ServiceUnavailable ---
//
// `GtsTypeSchemaNotFound` on `get_type_schema_by_uuid` means the row's
// `tenant_type_uuid` no longer resolves through the Types Registry.
// The SDK contract on `IdpTenantContext::tenant_type` requires a
// *resolved* chained `GtsSchemaId`, so the helper surfaces
// `ServiceUnavailable` rather than fabricating a placeholder; the
// cleanup pipeline (`reap_stuck_provisioning` / `hard_delete_batch`)
// already routes that variant through its existing `Defer` /
// `context_load_failed` arm. Recovery is a registry restore or schema
// backfill — not a silent fake.

#[tokio::test]
async fn load_tenant_context_defers_on_gts_type_schema_not_found() {
    let root = Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(Arc::new(UuidNotFoundTypesRegistry));

    let err = svc
        .load_tenant_context(root)
        .await
        .expect_err("catalog drift on tenant_type_uuid MUST surface as ServiceUnavailable");

    match &err {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert!(
                detail.contains("catalog drift")
                    && detail.contains("not registered in the Types Registry"),
                "ServiceUnavailable.detail must name the drift cause for operator \
                 correlation; got: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

// ---- H4: MAX_IDP_METADATA_BYTES boundary cap --------------------------
//
// The opaque idp-metadata blob is reshipped on every subsequent IdP
// call via `TenantContext::metadata`, so an unbounded payload
// amplifies per user-op (not per provisioning call). The
// `check_idp_metadata_size` helper enforces a 64 KiB cap on both
// the caller-supplied input AND the plugin-returned blob in the
// create-child saga (and in the bootstrap saga via the preflight +
// `handle_provision_success`).

#[tokio::test]
async fn create_tenant_rejects_oversize_provisioning_metadata_input() {
    use crate::domain::tenant::service::MAX_IDP_METADATA_BYTES;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xB100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo,
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    // Construct a blob whose serialised JSON exceeds the cap. A
    // single string of `MAX_IDP_METADATA_BYTES + 1` ASCII bytes
    // serialises to ~ `MAX_IDP_METADATA_BYTES + 3` bytes (two
    // wrapping quotes); fine for the cap check.
    let oversize = serde_json::json!("x".repeat(MAX_IDP_METADATA_BYTES + 1));
    let mut input = child_input(child, root);
    input.provisioning_metadata = Some(oversize);

    let err = svc
        .create_tenant(&ctx_for(root), input)
        .await
        .expect_err("oversize provisioning_metadata MUST reject");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("create_tenant.provisioning_metadata")
                    && detail.contains("byte AM boundary cap"),
                "Validation must name the cap source; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(
        idp.calls.lock().expect("lock").len(),
        0,
        "AM-side cap MUST short-circuit BEFORE the IdP round-trip"
    );
}

#[tokio::test]
async fn create_tenant_rejects_oversize_plugin_returned_metadata_and_compensates() {
    use crate::domain::tenant::service::MAX_IDP_METADATA_BYTES;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xB200);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    // Plugin returns an oversize blob on `provision_tenant`.
    let oversize = serde_json::json!("x".repeat(MAX_IDP_METADATA_BYTES + 1));
    idp.set_metadata(Some(oversize));
    let svc = TenantService::new(
        repo.clone(),
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    let err = svc
        .create_tenant(&ctx_for(root), child_input(child, root))
        .await
        .expect_err("oversize plugin-returned metadata MUST reject");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("create_tenant.idp_returned_metadata")
                    && detail.contains("byte AM boundary cap"),
                "Validation must name the cap source; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    // Best-effort compensation MUST run so the plugin-side state
    // does not orphan — same compensation rung as the upsert /
    // activation failure paths.
    assert_eq!(
        idp.deprovision_calls.lock().expect("lock").len(),
        1,
        "saga MUST attempt IdP compensation when the plugin-returned \
         metadata exceeds the AM cap"
    );
}

#[tokio::test]
async fn create_tenant_accepts_metadata_at_cap_boundary() {
    use crate::domain::tenant::service::MAX_IDP_METADATA_BYTES;

    let root = Uuid::from_u128(0x100);
    let child = Uuid::from_u128(0xB300);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let svc = TenantService::new(
        repo,
        idp.clone(),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig::default(),
    )
    .with_types_registry(::std::sync::Arc::new(ConstantTypesRegistry));

    // `MAX_IDP_METADATA_BYTES - 10` ASCII bytes wrapped in JSON
    // quotes lands well under the cap. Pinned that "at the cap"
    // is accepted (symmetric with the reject test).
    let near_cap = serde_json::json!("x".repeat(MAX_IDP_METADATA_BYTES - 10));
    let mut input = child_input(child, root);
    input.provisioning_metadata = Some(near_cap);

    let _ = svc
        .create_tenant(&ctx_for(root), input)
        .await
        .expect("metadata just under the cap MUST pass");
}
