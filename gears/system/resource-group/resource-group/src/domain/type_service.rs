// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-type-mgmt:p1
//! Domain service for GTS type management.
//!
//! Implements business rules: input validation, placement invariant,
//! hierarchy safety checks, and CRUD orchestration.

use std::sync::Arc;

use resource_group_sdk::models::{CreateTypeRequest, ResourceGroupType, UpdateTypeRequest};
use toolkit_db::secure::{DBRunner, TxConfig};
use toolkit_odata::{ODataQuery, Page};

use tracing::{debug, warn};

use crate::domain::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::repo::TypeRepositoryTrait;
#[allow(unused_imports)]
use crate::domain::validation;

// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1
/// Service for GTS type lifecycle management.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
pub struct TypeService<TR: TypeRepositoryTrait> {
    db: Arc<DbProvider>,
    type_repo: Arc<TR>,
}

impl<TR: TypeRepositoryTrait> TypeService<TR> {
    /// Create a new `TypeService` with the given database provider.
    #[must_use]
    pub fn new(db: Arc<DbProvider>, type_repo: Arc<TR>) -> Self {
        Self { db, type_repo }
    }

    // @cpt-flow:cpt-cf-resource-group-flow-type-mgmt-create-type:p1
    /// Create a new GTS type definition.
    ///
    /// The full INSERT-junction sequence (`type_repo.insert` →
    /// `insert_allowed_parent_types` → `insert_allowed_membership_types` →
    /// `load_full_type`) runs inside one `SERIALIZABLE` transaction so that
    /// a failure on any step rolls back the whole operation. Without this,
    /// a partial insert (e.g. type row written but parent-types junction
    /// failed) would leave the registry in an inconsistent state.
    pub async fn create_type(
        &self,
        req: CreateTypeRequest,
    ) -> Result<ResourceGroupType, DomainError> {
        // Pre-validation (pure, no DB) — runs outside the transaction.
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-2
        // Validate GTS type path format via `GtsTypePath` value object.
        validation::validate_type_code(&req.code)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-2
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-3
        // Validate placement invariant: `can_be_root OR len(allowed_parent_types) >= 1`.
        Self::validate_placement_invariant(req.can_be_root, &req.allowed_parent_types)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-3
        if let Some(ref schema) = req.metadata_schema {
            validation::validate_metadata_schema(schema)?;
        }
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5
        // FOR EACH parent_path in allowed_parent_types
        for parent_code in &req.allowed_parent_types {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5a
            // Validate parent_path has RG type prefix `gts.cf.core.rg.type.v1~`
            validation::validate_type_code(parent_code)?;
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5a
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6
        // FOR EACH membership_path in allowed_membership_types
        for membership_code in &req.allowed_membership_types {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6a
            // Validate membership_path is a syntactically valid GtsTypePath.
            // Per DESIGN.md, membership resource types are external domain
            // types (e.g. `gts.cf.core.idp.user.v1~`) and are NOT required
            // to carry the RG type-registry prefix.
            validation::validate_membership_type_code(membership_code)?;
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6a
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6

        let stored_schema =
            Self::build_stored_schema(req.can_be_root, req.metadata_schema.as_ref());
        let db = self.db.db();
        let type_repo = self.type_repo.clone();

        db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-8
                // IF unique constraint violation → RETURN TypeAlreadyExists with
                // conflicting schema_id. Performed in-tx so a concurrent create
                // cannot slip a duplicate row in between this read and the
                // insert below.
                if type_repo.find_by_code(tx, &req.code).await?.is_some() {
                    debug!(code = %req.code, "Type already exists, rejecting create");
                    return Err(DomainError::type_already_exists(&req.code));
                }
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-8

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4
                // IF allowed_parent_types is non-empty
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4a
                // DB: SELECT id FROM gts_type WHERE schema_id IN (allowed_parent_types)
                // — verify all referenced parent types exist
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4b
                // IF any parent type not found → RETURN Validation error with
                // missing type paths (handled by `resolve_ids` returning
                // `DomainError::validation`).
                // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5b
                // Verify parent_path exists in gts_type table (resolve_ids
                // returns a `validation` error listing missing codes).
                let parent_ids = if req.allowed_parent_types.is_empty() {
                    Vec::new()
                } else {
                    type_repo.resolve_ids(tx, &req.allowed_parent_types).await?
                };
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5b
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4b
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4a
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5
                // IF allowed_membership_types is non-empty
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5a
                // DB: SELECT id FROM gts_type WHERE schema_id IN (allowed_membership_types)
                // — verify all referenced membership types exist
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5b
                // IF any membership type not found → RETURN Validation error
                // with missing type paths.
                // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6b
                // Verify membership_path exists in gts_type table (resolve_ids
                // returns a `validation` error listing missing codes).
                let membership_ids = if req.allowed_membership_types.is_empty() {
                    Vec::new()
                } else {
                    type_repo
                        .resolve_ids(tx, &req.allowed_membership_types)
                        .await?
                };
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6b
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5b
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5a
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-6
                // Resolve GTS type path to SMALLINT surrogate ID at persistence
                // boundary (the `type_repo.insert` call below assigns the
                // surrogate id and the subsequent re-read returns it).
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-7
                // DB: INSERT INTO gts_type (schema_id, metadata_schema) — with
                // uniqueness constraint on schema_id.
                let type_model = type_repo
                    .insert(tx, &req.code, Some(&stored_schema))
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-7
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-6
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-9
                // DB: INSERT INTO gts_type_allowed_parent (type_id, parent_type_id)
                // for each allowed parent.
                type_repo
                    .insert_allowed_parent_types(tx, type_model.id, &parent_ids)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-9
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-10
                // DB: INSERT INTO gts_type_allowed_membership (type_id, membership_type_id)
                // for each allowed membership.
                type_repo
                    .insert_allowed_membership_types(tx, type_model.id, &membership_ids)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-10
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-11
                // RETURN created ResourceGroupType with schema_id,
                // allowed_parent_types, allowed_membership_types, can_be_root,
                // metadata_schema (loaded with junctions).
                // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-8
                // RETURN validated type definition (loaded with junctions).
                type_repo.load_full_type(tx, &type_model).await
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-8
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-11
            })
        })
        .await
    }

    /// Get a GTS type definition by its code (GTS type path).
    pub async fn get_type(&self, code: &str) -> Result<ResourceGroupType, DomainError> {
        let conn = self.db.conn()?;
        self.type_repo
            .find_by_code(&conn, code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(code))
    }

    /// List GTS type definitions with `OData` filtering and pagination.
    pub async fn list_types(
        &self,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, DomainError> {
        let conn = self.db.conn()?;
        self.type_repo.list_types(&conn, query).await
    }

    // @cpt-flow:cpt-cf-resource-group-flow-type-mgmt-update-type:p1
    /// Update a GTS type definition (full replacement).
    ///
    /// The `delete_allowed_*` / `insert_allowed_*` / `update_type` sequence
    /// runs inside one `SERIALIZABLE` transaction so a failure on any later
    /// step rolls back the partial junction rewrites — without it, a crash
    /// between the parent-types delete and the membership-types insert
    /// would leave the registry pointing at half the new definition.
    pub async fn update_type(
        &self,
        code: &str,
        req: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, DomainError> {
        // Pre-validation (pure, no DB) — runs outside the transaction.
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-4
        // Validate placement invariant on new values.
        Self::validate_placement_invariant(req.can_be_root, &req.allowed_parent_types)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-4
        for parent_code in &req.allowed_parent_types {
            validation::validate_type_code(parent_code)?;
        }
        for membership_code in &req.allowed_membership_types {
            validation::validate_membership_type_code(membership_code)?;
        }
        if let Some(ref schema) = req.metadata_schema {
            validation::validate_metadata_schema(schema)?;
        }

        let stored_schema =
            Self::build_stored_schema(req.can_be_root, req.metadata_schema.as_ref());
        let db = self.db.db();
        let type_repo = self.type_repo.clone();
        let code = code.to_owned();

        db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-2
                // DB: SELECT FROM gts_type WHERE schema_id = {code} — load existing type
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-3
                // IF type not found → RETURN NotFound
                let existing = type_repo
                    .find_by_code(tx, &code)
                    .await?
                    .ok_or_else(|| DomainError::type_not_found(&code))?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-3
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-2

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-5
                // Validate all referenced allowed_parent_types and
                // allowed_membership_types types exist (resolve_ids returns
                // a `validation` error listing missing codes).
                let parent_ids = if req.allowed_parent_types.is_empty() {
                    Vec::new()
                } else {
                    type_repo.resolve_ids(tx, &req.allowed_parent_types).await?
                };
                let membership_ids = if req.allowed_membership_types.is_empty() {
                    Vec::new()
                } else {
                    type_repo
                        .resolve_ids(tx, &req.allowed_membership_types)
                        .await?
                };
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-5

                let type_id = type_repo
                    .resolve_id(tx, &code)
                    .await?
                    .ok_or_else(|| DomainError::type_not_found(&code))?;

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-6
                // Invoke hierarchy safety check algorithm for
                // allowed_parent_types and can_be_root changes.
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-7
                // IF hierarchy safety check fails → RETURN
                // AllowedParentTypesViolation with violating group details
                // (returned by `check_hierarchy_safety`).
                Self::check_hierarchy_safety(&*type_repo, tx, type_id, &existing, &req).await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-7
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-6

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-8
                // DB: DELETE FROM gts_type_allowed_parent WHERE type_id = {id}
                // — clear old parents.
                type_repo.delete_allowed_parent_types(tx, type_id).await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-8
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-9
                // DB: INSERT INTO gts_type_allowed_parent — insert new parents.
                type_repo
                    .insert_allowed_parent_types(tx, type_id, &parent_ids)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-9
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-10
                // DB: DELETE FROM gts_type_allowed_membership WHERE type_id = {id}
                // — clear old memberships.
                type_repo
                    .delete_allowed_membership_types(tx, type_id)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-10
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-11
                // DB: INSERT INTO gts_type_allowed_membership — insert new
                // memberships.
                type_repo
                    .insert_allowed_membership_types(tx, type_id, &membership_ids)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-11

                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-12
                // DB: UPDATE gts_type SET metadata_schema = {new}, updated_at = now().
                let updated_model = type_repo
                    .update_type(tx, type_id, &code, Some(&stored_schema))
                    .await?;
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-12
                // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-13
                // RETURN updated ResourceGroupType (loaded with refreshed junctions).
                type_repo.load_full_type(tx, &updated_model).await
                // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-13
            })
        })
        .await
    }

    // @cpt-flow:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1
    /// Delete a GTS type definition.
    pub async fn delete_type(&self, code: &str) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-1
        // Actor sends DELETE /api/types-registry/v1/types/{code}
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-1
        let conn = self.db.conn()?;

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-2
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-3
        let type_id = self
            .type_repo
            .resolve_id(&conn, code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(code))?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-3
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-2

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-4
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-5
        // Check for active references
        let count = self.type_repo.count_groups_of_type(&conn, type_id).await?;
        if count > 0 {
            warn!(code = %code, count, "Cannot delete type: active group references exist");
            return Err(DomainError::conflict_active_references(format!(
                "Cannot delete type '{code}': {count} group(s) of this type exist"
            )));
        }
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-5
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-4

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-6
        self.type_repo.delete_by_id(&conn, type_id).await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-6
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-7
        Ok(())
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-delete-type:p1:inst-delete-type-7
    }

    // -- Validation helpers --

    fn validate_placement_invariant(
        can_be_root: bool,
        allowed_parent_types: &[String],
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-4
        if !can_be_root && allowed_parent_types.is_empty() {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-4a
            return Err(DomainError::validation(
                "Type must allow root placement or have at least one allowed parent",
            ));
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-4a
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-4
        Ok(())
    }

    /// Build the stored `metadata_schema` JSON with internal `__can_be_root` key.
    ///
    /// Whether this type starts a new tenant scope is no longer stored — it is
    /// derived at runtime from the type code prefix ([`TENANT_RG_TYPE_PATH`]).
    fn build_stored_schema(
        can_be_root: bool,
        metadata_schema: Option<&serde_json::Value>,
    ) -> serde_json::Value {
        let mut map = match metadata_schema {
            Some(serde_json::Value::Object(m)) => m.clone(),
            Some(v) => {
                let mut m = serde_json::Map::new();
                m.insert("__user_schema".to_owned(), v.clone());
                m
            }
            None => serde_json::Map::new(),
        };
        map.insert(
            "__can_be_root".to_owned(),
            serde_json::Value::Bool(can_be_root),
        );
        serde_json::Value::Object(map)
    }

    // @cpt-algo:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1
    async fn check_hierarchy_safety(
        type_repo: &TR,
        conn: &impl DBRunner,
        type_id: i16,
        existing: &ResourceGroupType,
        req: &UpdateTypeRequest,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-1
        // Compute removed parent types: old_allowed_parent_types - new_allowed_parent_types
        let removed_parents: Vec<String> = existing
            .allowed_parent_types
            .iter()
            .filter(|p| !req.allowed_parent_types.contains(p))
            .cloned()
            .collect();
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-1

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2
        if !removed_parents.is_empty() {
            // One call for every removed parent, instead of a `resolve_id`
            // plus a single-parent lookup per removed parent (N+1 audit
            // finding (b): two SELECTs per element of the request).
            let violations = type_repo
                .find_groups_violating_removed_parents(conn, type_id, &removed_parents)
                .await?;

            // Report the first removed parent, in request order, that has a
            // violation -- the same answer the per-parent loop gave, since
            // it iterated in that order and returned on its first hit.
            if let Some(removed_parent) = removed_parents
                .iter()
                .find(|p| violations.iter().any(|(code, _, _)| code == *p))
            {
                let names: Vec<String> = violations
                    .iter()
                    .filter(|(code, _, _)| code == removed_parent)
                    .map(|(_, _, name)| name.clone())
                    .collect();
                return Err(DomainError::allowed_parent_types_violation(format!(
                    "Cannot remove allowed parent '{removed_parent}': groups using this parent relationship: {}",
                    names.join(", ")
                )));
            }
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3
        // Check can_be_root change from true to false
        if existing.can_be_root && !req.can_be_root {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3a
            let root_groups = type_repo.find_root_groups_of_type(conn, type_id).await?;
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3a

            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3b
            if !root_groups.is_empty() {
                let names: Vec<String> = root_groups.iter().map(|(_, name)| name.clone()).collect();
                return Err(DomainError::allowed_parent_types_violation(format!(
                    "Cannot disable root placement: root groups of this type exist: {}",
                    names.join(", ")
                )));
            }
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3b
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-3

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-4
        // IF violations collected -> RETURN AllowedParentTypesViolation (handled inline above)
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-4

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-5
        Ok(())
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-5
    }
}
// @cpt-end:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1:inst-full
