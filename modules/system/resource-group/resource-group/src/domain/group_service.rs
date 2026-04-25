// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-entity-hier-entity-service:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-entity-hierarchy:p1
//! Domain service for resource group entity management.
//!
//! Implements business rules: type validation, parent compatibility,
//! cycle detection, closure table management, query profile enforcement,
//! and CRUD orchestration.
//!
//! All hierarchy-mutating operations (`create_group`, `update_group`,
//! `move_group`, `delete_group`) use `SERIALIZABLE` transactions with
//! bounded retry (max 3 attempts) to prevent phantom reads and ensure
//! closure table consistency under concurrent mutations.

use std::sync::Arc;

use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use modkit_db::secure::DBRunner;
use modkit_odata::{ODataQuery, Page};
use modkit_security::{SecurityContext, pep_properties};
use resource_group_sdk::TENANT_RG_TYPE_PATH;
use resource_group_sdk::models::{
    CreateGroupRequest, ResourceGroup, ResourceGroupWithDepth, UpdateGroupRequest,
};
use tracing::debug;
use uuid::Uuid;

use crate::domain::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::repo::{GroupRepositoryTrait, TypeRepositoryTrait};
use crate::domain::validation;

/// `AuthZ` resource type descriptor for resource groups.
pub const RG_GROUP_RESOURCE: ResourceType = ResourceType {
    name: "gts.x.system.rg.group.v1~",
    supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
};

/// Query profile configuration for depth/width limits.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct QueryProfile {
    /// Maximum depth allowed. `None` disables depth limit.
    pub max_depth: Option<u32>,
    /// Maximum width (children per parent) allowed. `None` disables width limit.
    pub max_width: Option<u32>,
}

impl Default for QueryProfile {
    fn default() -> Self {
        Self {
            max_depth: Some(10),
            max_width: None,
        }
    }
}

// @cpt-dod:cpt-cf-resource-group-dod-entity-hier-entity-service:p1
// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-tenant-scope:p1
/// Service for resource group entity lifecycle management.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
pub struct GroupService<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait> {
    db: Arc<DbProvider>,
    profile: QueryProfile,
    enforcer: PolicyEnforcer,
    group_repo: Arc<GR>,
    type_repo: Arc<TR>,
    types_registry: Arc<dyn types_registry_sdk::TypesRegistryClient>,
}

impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait> GroupService<GR, TR> {
    /// Create a new `GroupService` with the given database provider, query profile,
    /// and `PolicyEnforcer` for AuthZ-scoped queries.
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        profile: QueryProfile,
        enforcer: PolicyEnforcer,
        group_repo: Arc<GR>,
        type_repo: Arc<TR>,
        types_registry: Arc<dyn types_registry_sdk::TypesRegistryClient>,
    ) -> Self {
        Self {
            db,
            profile,
            enforcer,
            group_repo,
            type_repo,
            types_registry,
        }
    }

    // @cpt-flow:cpt-cf-resource-group-flow-entity-hier-create-group:p1
    /// Create a new resource group.
    ///
    /// Runs inside a `SERIALIZABLE` transaction with bounded retry (max 3 attempts)
    /// to ensure invariant checks and closure table mutations are atomic.
    pub async fn create_group(
        &self,
        ctx: &SecurityContext,
        req: CreateGroupRequest,
        tenant_id: Uuid,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-1
        // Pre-validation (stateless, outside transaction)
        validation::validate_type_code(&req.code)?;
        Self::validate_name(&req.name)?;

        // Derive `is_tenant` for AuthZ properties from the code prefix: any type
        // whose path starts with `TENANT_RG_TYPE_PATH` opens a new tenant scope.
        let is_tenant = req.code.starts_with(TENANT_RG_TYPE_PATH);

        // AuthZ gate with provisioning context
        let _scope =
            self.enforcer
                .access_scope_with(
                    ctx,
                    &RG_GROUP_RESOURCE,
                    "create",
                    None,
                    &authz_resolver_sdk::pep::enforcer::AccessRequest::default()
                        .resource_properties(std::collections::HashMap::from([
                            ("is_tenant".to_owned(), serde_json::Value::Bool(is_tenant)),
                            (
                                "parent_id".to_owned(),
                                req.parent_id.map_or(serde_json::Value::Null, |id| {
                                    serde_json::Value::String(id.to_string())
                                }),
                            ),
                        ])),
                )
                .await
                .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-1

        let profile = self.profile.clone();
        let db = self.db.db();
        let group_repo = self.group_repo.clone();
        let type_repo = self.type_repo.clone();
        let types_registry = self.types_registry.clone();

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-2
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-10
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-9
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-11
        db.transaction_serializable_with_retry(DomainError::is_serialization_failure, |tx| {
            let req = req.clone();
            let profile = profile.clone();
            let group_repo = group_repo.clone();
            let type_repo = type_repo.clone();
            let types_registry = types_registry.clone();
            Box::pin(async move {
                Self::create_group_inner(
                    &*group_repo,
                    &*type_repo,
                    tx,
                    &req,
                    tenant_id,
                    &profile,
                    &*types_registry,
                )
                .await
            })
        })
        .await
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-11
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-9
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-10
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-2
    }

    /// Get a resource group by ID (AuthZ-scoped).
    pub async fn get_group(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
    ) -> Result<ResourceGroup, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "get", Some(group_id))
            .await
            .map_err(DomainError::from)?;
        let conn = self.db.conn()?;
        self.group_repo
            .find_by_id(&conn, &scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))
    }

    /// List resource groups with `OData` filtering and pagination (AuthZ-scoped).
    pub async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "list", None)
            .await
            .map_err(DomainError::from)?;
        let conn = self.db.conn()?;
        self.group_repo.list_groups(&conn, &scope, query).await
    }

    // @cpt-flow:cpt-cf-resource-group-flow-entity-hier-update-group:p1
    /// Update a resource group (full replacement via PUT, AuthZ-scoped).
    ///
    /// Runs inside a `SERIALIZABLE` transaction with bounded retry (max 3 attempts)
    /// to ensure invariant checks, closure table mutations, and the update are atomic.
    pub async fn update_group(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        req: UpdateGroupRequest,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-1
        // Actor sends PUT /api/resource-group/v1/groups/{group_id}
        // AuthZ gate: verify the caller can update this group (tenant check).
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "update", Some(group_id))
            .await
            .map_err(DomainError::from)?;

        // Pre-validation (stateless, outside transaction).
        // Type is immutable on update — `UpdateGroupRequest` deliberately
        // does not carry a `code` field — so there is nothing to validate
        // syntactically here besides the display name.
        Self::validate_name(&req.name)?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-1

        let profile = self.profile.clone();
        let db = self.db.db();
        let group_repo = self.group_repo.clone();
        let type_repo = self.type_repo.clone();
        let types_registry = self.types_registry.clone();

        db.transaction_serializable_with_retry(DomainError::is_serialization_failure, |tx| {
            let req = req.clone();
            let scope = scope.clone();
            let profile = profile.clone();
            let group_repo = group_repo.clone();
            let type_repo = type_repo.clone();
            let types_registry = types_registry.clone();
            Box::pin(async move {
                Self::update_group_inner(
                    &*group_repo,
                    &*type_repo,
                    tx,
                    &scope,
                    group_id,
                    &req,
                    &profile,
                    &*types_registry,
                )
                .await
            })
        })
        .await
    }

    // @cpt-flow:cpt-cf-resource-group-flow-entity-hier-move-group:p1
    /// Move a group to a new parent (or make it a root).
    ///
    /// Runs inside a `SERIALIZABLE` transaction with bounded retry (max 3 attempts)
    /// to ensure cycle detection, invariant checks, and closure table rebuild are atomic.
    pub async fn move_group(
        &self,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-1
        // Actor sends PUT /api/resource-group/v1/groups/{group_id} with new hierarchy.parent_id
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-1
        let profile = self.profile.clone();
        let db = self.db.db();
        let group_repo = self.group_repo.clone();
        let type_repo = self.type_repo.clone();

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-2
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-12
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-11
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-13
        db.transaction_serializable_with_retry(DomainError::is_serialization_failure, |tx| {
            let profile = profile.clone();
            let group_repo = group_repo.clone();
            let type_repo = type_repo.clone();
            Box::pin(async move {
                Self::move_group_inner(
                    &*group_repo,
                    &*type_repo,
                    tx,
                    group_id,
                    new_parent_id,
                    &profile,
                )
                .await
            })
        })
        .await
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-13
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-11
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-12
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-2
    }

    // @cpt-flow:cpt-cf-resource-group-flow-entity-hier-delete-group:p1
    /// Delete a resource group (AuthZ-scoped).
    ///
    /// Runs inside a `SERIALIZABLE` transaction with bounded retry (max 3 attempts)
    /// to ensure reference checks and cascading deletes are atomic.
    pub async fn delete_group(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        force: bool,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-1
        // Actor sends DELETE /api/resource-group/v1/groups/{group_id}?force={true|false}
        // AuthZ gate: verify the caller can delete this group (tenant check).
        // Runs outside the transaction since AuthZ is idempotent.
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "delete", Some(group_id))
            .await
            .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-1

        let db = self.db.db();
        let group_repo = self.group_repo.clone();

        db.transaction_serializable_with_retry(DomainError::is_serialization_failure, |tx| {
            let scope = scope.clone();
            let group_repo = group_repo.clone();
            Box::pin(async move {
                Self::delete_group_inner(&*group_repo, tx, &scope, group_id, force).await
            })
        })
        .await
    }

    /// Get descendants of a group (depth >= 0, AuthZ-scoped).
    pub async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "list", Some(group_id))
            .await
            .map_err(DomainError::from)?;
        let conn = self.db.conn()?;
        // Scope-aware preflight: a cross-tenant id must look the same as a
        // non-existent id from the caller's viewpoint, otherwise we leak the
        // existence of cross-tenant roots (random id → 404, foreign id → 200
        // with empty page).
        self.group_repo
            .find_by_id(&conn, &scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;
        self.group_repo
            .get_descendants(&conn, &scope, group_id, query)
            .await
    }

    /// Get ancestors of a group (depth <= 0, AuthZ-scoped).
    pub async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_GROUP_RESOURCE, "list", Some(group_id))
            .await
            .map_err(DomainError::from)?;
        let conn = self.db.conn()?;
        // Scope-aware preflight: see comment in `get_group_descendants`.
        self.group_repo
            .find_by_id(&conn, &scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;
        self.group_repo
            .get_ancestors(&conn, &scope, group_id, query)
            .await
    }

    // -- Unscoped reads (for integration read service, bypasses AuthZ) --
    //
    // These methods are exposed via `ResourceGroupReadHierarchy` trait
    // (registered in ClientHub as `dyn ResourceGroupReadHierarchy`).
    // They use `AccessScope::allow_all()` — no tenant WHERE clause.
    //
    // This is by design (DESIGN §3.6): the AuthZ plugin is the primary
    // consumer of these reads. It cannot evaluate itself (circular dep).
    // In microservice deployments these endpoints are MTLS-only; in
    // monolith mode the in-process ClientHub path skips AuthZ entirely.
    //
    // SECURITY: do NOT expose these methods via REST handlers.
    // REST uses the scoped variants (`get_group_descendants` / `get_group_ancestors`).

    /// Get descendants without `AuthZ` enforcement (private API, no tenant scoping).
    pub async fn get_group_descendants_unscoped(
        &self,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let conn = self.db.conn()?;
        let scope = modkit_security::AccessScope::allow_all();
        self.group_repo
            .get_descendants(&conn, &scope, group_id, query)
            .await
    }

    /// Get ancestors without `AuthZ` enforcement (private API, no tenant scoping).
    ///
    /// Used by `ResourceGroupReadHierarchy` consumers (e.g., tenant-resolver plugin)
    /// that need full ancestor visibility regardless of the caller's tenant scope.
    pub async fn get_group_ancestors_unscoped(
        &self,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError> {
        let conn = self.db.conn()?;
        let scope = modkit_security::AccessScope::allow_all();
        self.group_repo
            .get_ancestors(&conn, &scope, group_id, query)
            .await
    }

    /// List groups without `AuthZ` enforcement (private API, no tenant scoping).
    ///
    /// Used by `ResourceGroupReadHierarchy::list_groups` consumers (e.g.,
    /// the tenant-resolver RG plugin's batch `get_tenants` path) which need
    /// to resolve groups by id/type predicates regardless of the caller's
    /// tenant scope. Mirrors the pattern of `get_group_*_unscoped`.
    pub async fn list_groups_unscoped(
        &self,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, DomainError> {
        let conn = self.db.conn()?;
        let scope = modkit_security::AccessScope::allow_all();
        self.group_repo.list_groups(&conn, &scope, query).await
    }

    /// Get a single group by id without `AuthZ` enforcement.
    ///
    /// **Internal API** — never expose this through a REST handler. Used by
    /// the seeding path (which runs at module init, before any caller
    /// security context exists) to check whether a seeded group is already
    /// present. Mirrors the pattern of the other `*_unscoped` methods.
    pub async fn get_group_unscoped(&self, group_id: Uuid) -> Result<ResourceGroup, DomainError> {
        let conn = self.db.conn()?;
        let scope = modkit_security::AccessScope::allow_all();
        self.group_repo
            .find_by_id(&conn, &scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))
    }

    /// Create a group without `AuthZ` enforcement.
    ///
    /// **Internal API** — never expose this through a REST handler. Used by
    /// the seeding path to provision required groups at module init, before
    /// any caller security context exists. Domain invariants (type
    /// validation, parent compatibility, tenant scoping, closure table
    /// maintenance) still run because this method calls the same
    /// `create_group_inner` as the public path; only the `PolicyEnforcer`
    /// gate is skipped.
    pub async fn create_group_unscoped(
        &self,
        req: CreateGroupRequest,
        tenant_id: Uuid,
    ) -> Result<ResourceGroup, DomainError> {
        validation::validate_type_code(&req.code)?;
        Self::validate_name(&req.name)?;

        let profile = self.profile.clone();
        let db = self.db.db();
        let group_repo = self.group_repo.clone();
        let type_repo = self.type_repo.clone();
        let types_registry = self.types_registry.clone();

        db.transaction_serializable_with_retry(DomainError::is_serialization_failure, |tx| {
            let req = req.clone();
            let profile = profile.clone();
            let group_repo = group_repo.clone();
            let type_repo = type_repo.clone();
            let types_registry = types_registry.clone();
            Box::pin(async move {
                Self::create_group_inner(
                    &*group_repo,
                    &*type_repo,
                    tx,
                    &req,
                    tenant_id,
                    &profile,
                    &*types_registry,
                )
                .await
            })
        })
        .await
    }

    // -- Transaction-inner implementations --

    /// Inner logic for `create_group`, runs inside a SERIALIZABLE transaction.
    #[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
    async fn create_group_inner(
        group_repo: &GR,
        type_repo: &TR,
        tx: &impl DBRunner,
        req: &CreateGroupRequest,
        tenant_id: Uuid,
        profile: &QueryProfile,
        types_registry: &dyn types_registry_sdk::TypesRegistryClient,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-3
        // Resolve type GTS path to surrogate ID; verify type exists
        let type_id = type_repo
            .resolve_id(tx, &req.code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(&req.code))?;

        let rg_type = type_repo
            .find_by_code(tx, &req.code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(&req.code))?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-3

        // Validate metadata against GTS type schema (applies to both root and child groups)
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5b
        validation::validate_metadata_via_gts(req.metadata.as_ref(), &req.code, types_registry)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5b

        // Determine effective tenant_id by code-prefix rule:
        // - code starts with TENANT_RG_TYPE_PATH → tenant_id = group.id (new scope)
        // - otherwise                           → tenant_id from caller / parent
        let group_id = req.id.unwrap_or_else(Uuid::now_v7);
        let is_tenant_type = req.code.starts_with(TENANT_RG_TYPE_PATH);
        let effective_tenant_id = if is_tenant_type { group_id } else { tenant_id };

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4
        if let Some(parent_id) = req.parent_id {
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4a
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4b
            let parent = group_repo
                .find_model_by_id(tx, parent_id)
                .await?
                .ok_or_else(|| DomainError::group_not_found(parent_id))?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4b
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4a

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4c
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4d
            let parent_type_path = Self::resolve_type_path_from_id(tx, parent.gts_type_id).await?;
            if !rg_type.allowed_parent_types.contains(&parent_type_path) {
                return Err(DomainError::invalid_parent_type(format!(
                    "Type '{}' does not allow parent type '{}'",
                    req.code, parent_type_path
                )));
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4d
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4c

            // @cpt-algo:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-1
            // Extract caller effective tenant scope from SecurityContext.subject_tenant_id
            // (tenant_id is passed as parameter from caller's context)
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-1
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-2
            // IF caller is privileged platform-admin -> pass (but data invariants still checked)
            // (platform-admin bypass handled by middleware; data invariants enforced below)
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-2
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-3
            // Validate tenant compatibility (child must be same tenant as parent)
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-4
            // IF membership write: validate target group's tenant_id is compatible
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-4
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-5
            // Skip tenant enforcement for tenant-typed groups — they intentionally
            // create a new tenant scope (tenant_id = group.id != parent.tenant_id).
            if !is_tenant_type && parent.tenant_id != tenant_id {
                return Err(DomainError::validation(format!(
                    "Child group tenant_id ({tenant_id}) must match parent tenant_id ({})",
                    parent.tenant_id
                )));
            }
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-5
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-3
            // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-6
            // RETURN pass (tenant enforcement passed)
            // @cpt-end:cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement:p1:inst-tenant-enforce-6

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4e
            // Check query profile: depth limit
            if let Some(max_depth) = profile.max_depth {
                let parent_depth = group_repo.get_depth(tx, parent_id).await?;
                #[allow(clippy::cast_possible_wrap)]
                if parent_depth + 1 >= max_depth as i32 {
                    return Err(DomainError::limit_violation(format!(
                        "Depth limit exceeded: adding child at depth {} exceeds max_depth {}",
                        parent_depth + 1,
                        max_depth
                    )));
                }
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4e

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4f
            // Check query profile: width limit
            if let Some(max_width) = profile.max_width {
                let sibling_count = group_repo.count_children(tx, parent_id).await?;
                if sibling_count >= u64::from(max_width) {
                    return Err(DomainError::limit_violation(format!(
                        "Width limit exceeded: parent already has {sibling_count} children, max_width is {max_width}"
                    )));
                }
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4f
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-4

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-6
            // Insert group
            let _model = group_repo
                .insert(
                    tx,
                    group_id,
                    Some(parent_id),
                    type_id,
                    &req.name,
                    req.metadata.as_ref(),
                    effective_tenant_id,
                )
                .await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-6

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-7
            // Insert closure: self-row
            group_repo.insert_closure_self_row(tx, group_id).await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-7

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-8
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-8a
            // Insert ancestor closure rows from parent's ancestors with depth+1
            group_repo
                .insert_ancestor_closure_rows(tx, group_id, parent_id)
                .await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-8a
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-8

            let sys = modkit_security::AccessScope::allow_all();
            group_repo
                .find_by_id(tx, &sys, group_id)
                .await?
                .ok_or_else(|| DomainError::database("Insert succeeded but group not found"))
        } else {
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5a
            // Root group: validate can_be_root
            if !rg_type.can_be_root {
                return Err(DomainError::invalid_parent_type(format!(
                    "Type '{}' cannot be a root group (can_be_root=false)",
                    req.code
                )));
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5a

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5c
            // Tenant-root uniqueness: at most one tenant-type group may be a
            // forest root. `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`.
            if is_tenant_type
                && let Some(existing_root_id) = group_repo
                    .find_root_id_with_type_prefix(tx, TENANT_RG_TYPE_PATH)
                    .await?
            {
                return Err(DomainError::tenant_root_already_exists(format!(
                    "Cannot create tenant-type root '{}' ({}): tenant root already exists (id={})",
                    req.name, req.code, existing_root_id
                )));
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5c
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-create-group:p1:inst-create-group-5

            // Insert group
            let _model = group_repo
                .insert(
                    tx,
                    group_id,
                    None,
                    type_id,
                    &req.name,
                    req.metadata.as_ref(),
                    effective_tenant_id,
                )
                .await?;

            // Insert closure: self-row only
            group_repo.insert_closure_self_row(tx, group_id).await?;

            let sys = modkit_security::AccessScope::allow_all();
            group_repo
                .find_by_id(tx, &sys, group_id)
                .await?
                .ok_or_else(|| DomainError::database("Insert succeeded but group not found"))
        }
    }

    /// Inner logic for `update_group`, runs inside a SERIALIZABLE transaction.
    ///
    /// **Type immutability.** A group's GTS type is fixed at creation —
    /// `UpdateGroupRequest` does not carry a `code` field. The existing
    /// `gts_type_id` is reused unchanged for the persisted update, so all
    /// type-driven validation (allowed parents/children, tenant-root rule,
    /// metadata schema lookup) is anchored on the existing type, not on a
    /// caller-supplied one.
    ///
    /// **Tenant immutability.** A group's `tenant_id` is also fixed at
    /// creation. Reparenting is therefore allowed only **within the same
    /// tenant** — the new parent's `tenant_id` must equal the group's
    /// `existing.tenant_id`, otherwise the move is rejected with the same
    /// rule `create_group_inner` uses for non-tenant children. Tenant-type
    /// roots already have `tenant_id = group_id`, so the same equality check
    /// trivially holds for them as well.
    #[allow(clippy::too_many_arguments)]
    async fn update_group_inner(
        group_repo: &GR,
        type_repo: &TR,
        tx: &impl DBRunner,
        scope: &modkit_security::AccessScope,
        group_id: Uuid,
        req: &UpdateGroupRequest,
        profile: &QueryProfile,
        types_registry: &dyn types_registry_sdk::TypesRegistryClient,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-2
        // DB: SELECT FROM resource_group WHERE id = {group_id} -- load existing group
        group_repo
            .find_by_id(tx, scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;

        let existing = group_repo
            .find_model_by_id(tx, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-2

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-3
        // IF group not found -> RETURN NotFound (handled by ok_or_else above)
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-3

        // Type is immutable on update — reuse the existing `gts_type_id` and
        // resolve the type definition for `move_group_internal_impl`'s
        // parent-compatibility check below.
        let existing_type_path = Self::resolve_type_path_from_id(tx, existing.gts_type_id).await?;
        let rg_type = type_repo
            .find_by_code(tx, &existing_type_path)
            .await?
            .ok_or_else(|| DomainError::type_not_found(&existing_type_path))?;

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-4e
        validation::validate_metadata_via_gts(
            req.metadata.as_ref(),
            &existing_type_path,
            types_registry,
        )
        .await?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-4e

        // Cross-tenant parent change is forbidden. `tenant_id` is established
        // at creation and never rewritten — see the function-level doc above
        // for the invariant. Mirror `create_group_inner`'s tenant-scope
        // enforcement for non-tenant children. (Tenant-type roots have
        // `tenant_id == group_id` by construction; reparenting one under a
        // different parent is also rejected here because the equality check
        // would fail.)
        if let Some(new_parent_id) = req.parent_id
            && new_parent_id != existing.parent_id.unwrap_or_default()
        {
            let new_parent = group_repo
                .find_model_by_id(tx, new_parent_id)
                .await?
                .ok_or_else(|| DomainError::group_not_found(new_parent_id))?;
            if new_parent.tenant_id != existing.tenant_id {
                return Err(DomainError::validation(format!(
                    "Cannot move group {group_id} to a parent in a different tenant \
                     ({} → {}); cross-tenant moves are not supported",
                    existing.tenant_id, new_parent.tenant_id
                )));
            }
        }

        let parent_changed = existing.parent_id != req.parent_id;
        if parent_changed {
            // Delegate to move logic (cycle detection + closure rebuild).
            // Type stays the same, so use the resolved `rg_type` for parent
            // compatibility checks inside the move helper.
            Self::move_group_internal_impl(
                group_repo,
                tx,
                group_id,
                req.parent_id,
                &rg_type,
                profile,
            )
            .await?;
        }

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-5
        // Persist name/parent/metadata. `gts_type_id` is reused from the
        // existing row — type is immutable on update.
        let _model = group_repo
            .update(
                tx,
                group_id,
                req.parent_id,
                existing.gts_type_id,
                &req.name,
                req.metadata.as_ref(),
            )
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-5

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-6
        let sys = modkit_security::AccessScope::allow_all();
        group_repo
            .find_by_id(tx, &sys, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-update-group:p1:inst-update-group-6
    }

    /// Inner logic for `move_group`, runs inside a SERIALIZABLE transaction.
    async fn move_group_inner(
        group_repo: &GR,
        type_repo: &TR,
        tx: &impl DBRunner,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
        profile: &QueryProfile,
    ) -> Result<ResourceGroup, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-3
        // Load group and new parent in transaction
        let existing = group_repo
            .find_model_by_id(tx, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;

        let type_path = Self::resolve_type_path_from_id(tx, existing.gts_type_id).await?;
        let rg_type = type_repo
            .find_by_code(tx, &type_path)
            .await?
            .ok_or_else(|| DomainError::type_not_found(&type_path))?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-3

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-4
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-5
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-6
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-7
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-8
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-9
        // Cycle detect, type compat, profile enforce, closure rebuild
        Self::move_group_internal_impl(group_repo, tx, group_id, new_parent_id, &rg_type, profile)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-9
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-8
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-7
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-6
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-5
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-4

        // Cross-tenant moves are forbidden (`tenant_id` is immutable per the
        // module-wide invariant). Reject the move when the new parent lives
        // in a different tenant than the moved group; tenant-type roots have
        // `tenant_id == group_id`, so the equality check covers them too.
        if let Some(new_parent_id) = new_parent_id {
            let new_parent = group_repo
                .find_model_by_id(tx, new_parent_id)
                .await?
                .ok_or_else(|| DomainError::group_not_found(new_parent_id))?;
            if new_parent.tenant_id != existing.tenant_id {
                return Err(DomainError::validation(format!(
                    "Cannot move group {group_id} to a parent in a different tenant \
                     ({} → {}); cross-tenant moves are not supported",
                    existing.tenant_id, new_parent.tenant_id
                )));
            }
        }

        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-10
        // Update parent_id on the group. Type and tenant_id are immutable —
        // both reuse the existing row's values.
        group_repo
            .update(
                tx,
                group_id,
                new_parent_id,
                existing.gts_type_id,
                &existing.name,
                existing.metadata.as_ref(),
            )
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-move-group:p1:inst-move-group-10

        let sys = modkit_security::AccessScope::allow_all();
        group_repo
            .find_by_id(tx, &sys, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))
    }

    /// Inner logic for `delete_group`, runs inside a SERIALIZABLE transaction.
    async fn delete_group_inner(
        group_repo: &GR,
        tx: &impl DBRunner,
        scope: &modkit_security::AccessScope,
        group_id: Uuid,
        force: bool,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-2
        // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-3
        // DB: SELECT FROM resource_group WHERE id = {group_id}
        group_repo
            .find_by_id(tx, scope, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;

        let _existing = group_repo
            .find_model_by_id(tx, group_id)
            .await?
            .ok_or_else(|| DomainError::group_not_found(group_id))?;
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-3
        // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-2

        if force {
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5a
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5b
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5c
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5d
            // Force delete: cascade entire subtree + memberships + closure
            #[allow(clippy::let_and_return)]
            let result = Self::force_delete_subtree(group_repo, tx, group_id).await;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5d
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5c
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5b
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5a
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-5
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-7
            result
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-7
        } else {
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4
            // Non-force: check children and memberships
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4a
            let children = Self::get_direct_children(tx, group_id).await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4a
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4b
            let has_memberships = group_repo.has_memberships(tx, group_id).await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4b
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4c
            if !children.is_empty() {
                return Err(DomainError::conflict_active_references(format!(
                    "Cannot delete group '{group_id}': has {} child group(s). Use force=true to cascade.",
                    children.len()
                )));
            }

            if has_memberships {
                return Err(DomainError::conflict_active_references(format!(
                    "Cannot delete group '{group_id}': has active memberships. Use force=true to cascade."
                )));
            }
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4c
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-4

            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6a
            // Delete closure rows, then the group
            group_repo.delete_all_closure_rows(tx, group_id).await?;
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6a
            // @cpt-begin:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6b
            group_repo.delete_by_id(tx, group_id).await
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6b
            // @cpt-end:cpt-cf-resource-group-flow-entity-hier-delete-group:p1:inst-delete-group-6
        }
    }

    // -- Internal helpers --

    // @cpt-algo:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1
    // @cpt-algo:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1
    /// Internal move logic shared between `move_group` and `update_group`.
    ///
    /// Performs cycle detection, type compatibility checks, query profile
    /// enforcement, and closure table rebuild. Must be called within a
    /// SERIALIZABLE transaction.
    #[allow(clippy::cognitive_complexity)]
    async fn move_group_internal_impl(
        group_repo: &GR,
        conn: &impl DBRunner,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
        rg_type: &resource_group_sdk::ResourceGroupType,
        profile: &QueryProfile,
    ) -> Result<(), DomainError> {
        if let Some(new_pid) = new_parent_id {
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-1
            // Cycle detection: self-parent check (covered by is_descendant via self-row)
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-1
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-2
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-3
            let is_desc = group_repo.is_descendant(conn, group_id, new_pid).await?;
            if is_desc {
                debug!(group_id = %group_id, new_parent = %new_pid, "Cycle detected in move_group");
                return Err(DomainError::cycle_detected(format!(
                    "Cannot move group '{group_id}' under '{new_pid}': would create a cycle"
                )));
            }
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-3
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-2

            // Validate parent type compatibility
            let parent = group_repo
                .find_model_by_id(conn, new_pid)
                .await?
                .ok_or_else(|| DomainError::group_not_found(new_pid))?;

            let parent_type_path =
                Self::resolve_type_path_from_id(conn, parent.gts_type_id).await?;
            if !rg_type.allowed_parent_types.contains(&parent_type_path) {
                return Err(DomainError::invalid_parent_type(format!(
                    "Type '{}' does not allow parent type '{}'",
                    rg_type.code, parent_type_path
                )));
            }

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-4
            // Cycle detection passed
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-cycle-detect:p1:inst-cycle-4

            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-1
            // Load profile config: max_depth (optional), max_width (optional)
            // (profile is passed as parameter with max_depth and max_width)
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-1

            // Check query profile: depth limit
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2
            if let Some(max_depth) = profile.max_depth {
                // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2a
                let parent_depth = group_repo.get_depth(conn, new_pid).await?;
                // Check depth of deepest descendant of moved node
                let subtree_descendants = group_repo.get_descendant_ids(conn, group_id).await?;
                let mut max_subtree_depth = 0i32;
                for desc_id in &subtree_descendants {
                    // Internal depth within the subtree
                    let is_desc_result = group_repo.is_descendant(conn, group_id, *desc_id).await?;
                    if is_desc_result {
                        // Get the depth of this descendant relative to the moved group
                        // by looking at the closure table
                        let depth = Self::get_relative_depth(conn, group_id, *desc_id).await?;
                        if depth > max_subtree_depth {
                            max_subtree_depth = depth;
                        }
                    }
                }
                let new_deepest = parent_depth + 1 + max_subtree_depth;
                // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2a
                // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2b
                #[allow(clippy::cast_possible_wrap)]
                if new_deepest >= max_depth as i32 {
                    debug!(group_id = %group_id, new_deepest, max_depth, "Depth limit exceeded on move");
                    return Err(DomainError::limit_violation(format!(
                        "Depth limit exceeded: moving subtree would create depth {new_deepest}, max_depth is {max_depth}"
                    )));
                }
                // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2b
            }
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-2

            // Check query profile: width limit
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3
            if let Some(max_width) = profile.max_width {
                // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3a
                let sibling_count = group_repo.count_children(conn, new_pid).await?;
                // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3a
                // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3b
                if sibling_count >= u64::from(max_width) {
                    return Err(DomainError::limit_violation(format!(
                        "Width limit exceeded: new parent already has {sibling_count} children, max_width is {max_width}"
                    )));
                }
                // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3b
            }
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-3
            // @cpt-begin:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-4
            // Profile checks passed
            // @cpt-end:cpt-cf-resource-group-algo-entity-hier-enforce-query-profile:p1:inst-profile-4
        } else {
            // Moving to root: validate can_be_root + tenant-root uniqueness.
            if !rg_type.can_be_root {
                return Err(DomainError::invalid_parent_type(format!(
                    "Type '{}' cannot be a root group (can_be_root=false)",
                    rg_type.code
                )));
            }

            // Tenant-root uniqueness: at most one tenant-type group may be a
            // forest root. Mirrors the guard in `create_group_inner` —
            // `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`. We
            // exclude the moved group itself so a no-op move (already root)
            // does not falsely fire.
            if rg_type.code.starts_with(TENANT_RG_TYPE_PATH)
                && let Some(existing_root_id) = group_repo
                    .find_root_id_with_type_prefix(conn, TENANT_RG_TYPE_PATH)
                    .await?
                && existing_root_id != group_id
            {
                return Err(DomainError::tenant_root_already_exists(format!(
                    "Cannot move tenant-type group '{}' ({group_id}) to root: tenant root already exists (id={existing_root_id})",
                    rg_type.code
                )));
            }
        }

        // Rebuild closure table for the subtree
        group_repo
            .rebuild_subtree_closure(conn, group_id, new_parent_id)
            .await?;

        Ok(())
    }

    /// Force-delete an entire subtree (group + descendants + memberships + closure).
    async fn force_delete_subtree(
        group_repo: &GR,
        conn: &impl DBRunner,
        root_id: Uuid,
    ) -> Result<(), DomainError> {
        // Get all descendants
        let descendant_ids = group_repo.get_descendant_ids(conn, root_id).await?;

        // Delete in reverse order (leaves first)
        let mut all_ids = vec![root_id];
        all_ids.extend(descendant_ids);

        // Delete memberships and closure rows for all nodes
        for &gid in all_ids.iter().rev() {
            group_repo.delete_memberships(conn, gid).await?;
            group_repo.delete_all_closure_rows(conn, gid).await?;
        }

        // Delete group entities in reverse order (leaves first)
        for &gid in all_ids.iter().rev() {
            group_repo.delete_by_id(conn, gid).await?;
        }

        Ok(())
    }

    /// Get direct children of a group.
    async fn get_direct_children(
        conn: &impl DBRunner,
        parent_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::entity::resource_group::Model>, DomainError> {
        use modkit_db::secure::SecureEntityExt;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let scope = modkit_security::AccessScope::allow_all();
        crate::infra::storage::entity::resource_group::Entity::find()
            .filter(crate::infra::storage::entity::resource_group::Column::ParentId.eq(parent_id))
            .secure()
            .scope_with(&scope)
            .all(conn)
            .await
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// Get relative depth between an ancestor and descendant via closure table.
    async fn get_relative_depth(
        conn: &impl DBRunner,
        ancestor_id: Uuid,
        descendant_id: Uuid,
    ) -> Result<i32, DomainError> {
        use modkit_db::secure::SecureEntityExt;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let scope = modkit_security::AccessScope::allow_all();
        let row = crate::infra::storage::entity::resource_group_closure::Entity::find()
            .filter(
                crate::infra::storage::entity::resource_group_closure::Column::AncestorId
                    .eq(ancestor_id),
            )
            .filter(
                crate::infra::storage::entity::resource_group_closure::Column::DescendantId
                    .eq(descendant_id),
            )
            .secure()
            .scope_with(&scope)
            .one(conn)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(row.map_or(0, |r| r.depth))
    }

    /// Resolve a type ID to its GTS path.
    async fn resolve_type_path_from_id(
        conn: &impl DBRunner,
        type_id: i16,
    ) -> Result<String, DomainError> {
        use modkit_db::secure::SecureEntityExt;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let scope = modkit_security::AccessScope::allow_all();
        let model = crate::infra::storage::entity::gts_type::Entity::find()
            .filter(crate::infra::storage::entity::gts_type::Column::Id.eq(type_id))
            .secure()
            .scope_with(&scope)
            .one(conn)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?
            .ok_or_else(|| DomainError::database(format!("Type ID {type_id} not found")))?;
        Ok(model.schema_id)
    }

    fn validate_name(name: &str) -> Result<(), DomainError> {
        if name.is_empty() || name.len() > 255 {
            return Err(DomainError::validation(
                "Group name must be between 1 and 255 characters",
            ));
        }
        Ok(())
    }
}
// @cpt-end:cpt-cf-resource-group-dod-entity-hier-entity-service:p1:inst-full
