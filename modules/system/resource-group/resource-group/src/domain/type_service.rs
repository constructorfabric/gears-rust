// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-type-mgmt:p1
//! Domain service for GTS type management.
//!
//! Implements business rules: input validation, placement invariant,
//! hierarchy safety checks, and CRUD orchestration.

use std::sync::Arc;

use modkit_db::secure::DBRunner;
use modkit_odata::{ODataQuery, Page};
use resource_group_sdk::models::{CreateTypeRequest, ResourceGroupType, UpdateTypeRequest};

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
    pub async fn create_type(
        &self,
        req: CreateTypeRequest,
    ) -> Result<ResourceGroupType, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-1
        // Actor sends POST /api/types-registry/v1/types
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-1
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-2
        validation::validate_type_code(&req.code)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-2
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-3
        Self::validate_placement_invariant(req.can_be_root, &req.allowed_parent_types)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-3

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7
        if let Some(ref schema) = req.metadata_schema {
            validation::validate_metadata_schema(schema)?;
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7

        let conn = self.db.conn()?;

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-6
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-7
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-8
        // Check uniqueness
        let existing = self.type_repo.find_by_code(&conn, &req.code).await?;
        if existing.is_some() {
            debug!(code = %req.code, "Type already exists, rejecting create");
            return Err(DomainError::type_already_exists(&req.code));
        }
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-8

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4a
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4b
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5
        // Validate and resolve allowed_parent_types references
        let parent_ids = if req.allowed_parent_types.is_empty() {
            Vec::new()
        } else {
            for parent_code in &req.allowed_parent_types {
                // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5a
                validation::validate_type_code(parent_code)?;
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5a
            }
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5b
            self.type_repo
                .resolve_ids(&conn, &req.allowed_parent_types)
                .await?
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5b
        };
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-5
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4b
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4a
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-4

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5a
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5b
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6
        // Validate and resolve allowed_membership_types references
        let membership_ids = if req.allowed_membership_types.is_empty() {
            Vec::new()
        } else {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6a
            // Validate membership_path is a valid GtsTypePath (no RG prefix requirement)
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6a
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6b
            self.type_repo
                .resolve_ids(&conn, &req.allowed_membership_types)
                .await?
            // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6b
        };
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-6
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5b
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5a
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-5

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-8
        // RETURN validated type definition (persisting below)
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-8

        // Build stored schema with embedded __can_be_root
        let stored_schema =
            Self::build_stored_schema(req.can_be_root, req.metadata_schema.as_ref());

        // Persist the type
        let type_model = self
            .type_repo
            .insert(&conn, &req.code, Some(&stored_schema))
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-7
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-6

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-9
        // Insert junction entries
        self.type_repo
            .insert_allowed_parent_types(&conn, type_model.id, &parent_ids)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-9
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-10
        self.type_repo
            .insert_allowed_membership_types(&conn, type_model.id, &membership_ids)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-10

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-11
        // Load and return the full type
        self.type_repo.load_full_type(&conn, &type_model).await
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-11
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
    pub async fn update_type(
        &self,
        code: &str,
        req: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-1
        // Actor sends PUT /api/types-registry/v1/types/{code}
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-1
        let conn = self.db.conn()?;

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-2
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-3
        // Load existing type
        let existing = self
            .type_repo
            .find_by_code(&conn, code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(code))?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-3
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-2

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-4
        // Validate placement invariant on new values
        Self::validate_placement_invariant(req.can_be_root, &req.allowed_parent_types)?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-4

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-5
        // Validate and resolve references
        let parent_ids = if req.allowed_parent_types.is_empty() {
            Vec::new()
        } else {
            for parent_code in &req.allowed_parent_types {
                validation::validate_type_code(parent_code)?;
            }
            self.type_repo
                .resolve_ids(&conn, &req.allowed_parent_types)
                .await?
        };

        let membership_ids = if req.allowed_membership_types.is_empty() {
            Vec::new()
        } else {
            self.type_repo
                .resolve_ids(&conn, &req.allowed_membership_types)
                .await?
        };
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-5

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7
        if let Some(ref schema) = req.metadata_schema {
            validation::validate_metadata_schema(schema)?;
        }
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7

        // Resolve our own ID
        let type_id = self
            .type_repo
            .resolve_id(&conn, code)
            .await?
            .ok_or_else(|| DomainError::type_not_found(code))?;

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-6
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-7
        // Hierarchy safety check
        Self::check_hierarchy_safety(&*self.type_repo, &conn, type_id, &existing, &req).await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-7
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-6

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-8
        // Clear old junction entries, insert new ones, update type
        self.type_repo
            .delete_allowed_parent_types(&conn, type_id)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-8
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-9
        self.type_repo
            .insert_allowed_parent_types(&conn, type_id, &parent_ids)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-9

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-10
        self.type_repo
            .delete_allowed_membership_types(&conn, type_id)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-10
        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-11
        self.type_repo
            .insert_allowed_membership_types(&conn, type_id, &membership_ids)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-11

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-12
        let stored_schema =
            Self::build_stored_schema(req.can_be_root, req.metadata_schema.as_ref());
        let updated_model = self
            .type_repo
            .update_type(&conn, type_id, code, Some(&stored_schema))
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-12

        // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-13
        self.type_repo.load_full_type(&conn, &updated_model).await
        // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-13
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
        let removed_parents: Vec<&String> = existing
            .allowed_parent_types
            .iter()
            .filter(|p| !req.allowed_parent_types.contains(p))
            .collect();
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-1

        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2
        for removed_parent in &removed_parents {
            // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2a
            let parent_id = type_repo.resolve_id(conn, removed_parent).await?;
            if let Some(parent_id) = parent_id {
                let violations = type_repo
                    .find_groups_using_parent_type(conn, type_id, parent_id)
                    .await?;
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2a

                // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2b
                if !violations.is_empty() {
                    let names: Vec<String> =
                        violations.iter().map(|(_, name)| name.clone()).collect();
                    return Err(DomainError::allowed_parent_types_violation(format!(
                        "Cannot remove allowed parent '{removed_parent}': groups using this parent relationship: {}",
                        names.join(", ")
                    )));
                }
                // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-check-hierarchy-safety:p1:inst-hier-check-2b
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
