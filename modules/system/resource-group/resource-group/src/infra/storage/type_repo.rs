// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1
//! Persistence layer for GTS type management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths.

use async_trait::async_trait;
use modkit_db::odata::{LimitCfg, paginate_odata};
use modkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use modkit_odata::{ODataQuery, Page, SortDir};
use modkit_security::AccessScope;
use resource_group_sdk::ResourceGroupType;
use resource_group_sdk::odata::TypeFilterField;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::domain::error::DomainError;
use crate::domain::repo::TypeRepositoryTrait;
use crate::infra::storage::entity::{
    gts_type::{self, Entity as GtsTypeEntity},
    gts_type_allowed_membership::{self, Entity as AllowedMembershipEntity},
    gts_type_allowed_parent::{self, Entity as AllowedParentEntity},
    resource_group::{self as rg_entity, Entity as ResourceGroupEntity},
};
use crate::infra::storage::odata_mapper::TypeODataMapper;

/// Default `OData` pagination limits for types.
const TYPE_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// System-level access scope (no tenant/resource filtering).
fn system_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Repository for GTS type persistence operations.
pub struct TypeRepository;

impl TypeRepository {
    /// Resolve a GTS type path to its surrogate SMALLINT ID (static helper for filter resolution).
    pub async fn resolve_id(db: &impl DBRunner, code: &str) -> Result<Option<i16>, DomainError> {
        let scope = system_scope();
        let result = GtsTypeEntity::find()
            .filter(gts_type::Column::SchemaId.eq(code))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(result.map(|m| m.id))
    }

    /// Resolve allowed parent SMALLINT IDs to string paths.
    async fn load_allowed_parent_types(
        db: &impl DBRunner,
        type_id: i16,
    ) -> Result<Vec<String>, DomainError> {
        let scope = system_scope();
        let parents = AllowedParentEntity::find()
            .filter(gts_type_allowed_parent::Column::TypeId.eq(type_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let parent_ids: Vec<i16> = parents.into_iter().map(|m| m.parent_type_id).collect();

        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }

        let parent_types = GtsTypeEntity::find()
            .filter(gts_type::Column::Id.is_in(parent_ids))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let mut codes: Vec<String> = parent_types.into_iter().map(|m| m.schema_id).collect();
        codes.sort();
        Ok(codes)
    }

    /// Resolve allowed membership SMALLINT IDs to string paths.
    async fn load_allowed_membership_types(
        db: &impl DBRunner,
        type_id: i16,
    ) -> Result<Vec<String>, DomainError> {
        let scope = system_scope();
        let memberships = AllowedMembershipEntity::find()
            .filter(gts_type_allowed_membership::Column::TypeId.eq(type_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let membership_ids: Vec<i16> = memberships
            .into_iter()
            .map(|m| m.membership_type_id)
            .collect();

        if membership_ids.is_empty() {
            return Ok(Vec::new());
        }

        let membership_types = GtsTypeEntity::find()
            .filter(gts_type::Column::Id.is_in(membership_ids))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let mut codes: Vec<String> = membership_types.into_iter().map(|m| m.schema_id).collect();
        codes.sort();
        Ok(codes)
    }

    /// Find the raw model by code. Used to re-read a row immediately after
    /// `INSERT … RETURNING`-less writes (insert/update); returns a
    /// `DomainError::Database` if the row is unexpectedly missing — i.e. the
    /// write committed but the row vanished (possible only under concurrent
    /// delete with the same `schema_id`).
    async fn find_model_by_code(
        db: &impl DBRunner,
        code: &str,
    ) -> Result<gts_type::Model, DomainError> {
        let scope = system_scope();
        GtsTypeEntity::find()
            .filter(gts_type::Column::SchemaId.eq(code))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?
            .ok_or_else(|| {
                DomainError::database(format!(
                    "GTS type row with schema_id={code} not found after write (concurrent delete?)"
                ))
            })
    }
}

#[async_trait]
impl TypeRepositoryTrait for TypeRepository {
    /// Load a full type by its `schema_id` (GTS type path), resolving all
    /// junction table references from SMALLINT IDs to string paths.
    async fn find_by_code<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<ResourceGroupType>, DomainError> {
        let scope = system_scope();
        let type_model = GtsTypeEntity::find()
            .filter(gts_type::Column::SchemaId.eq(code))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let Some(type_model) = type_model else {
            return Ok(None);
        };

        self.load_full_type(db, &type_model).await.map(Some)
    }

    /// Load a full type by its surrogate SMALLINT ID.
    async fn load_full_type_by_id<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<ResourceGroupType, DomainError> {
        let scope = system_scope();
        let type_model = GtsTypeEntity::find()
            .filter(gts_type::Column::Id.eq(type_id))
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?
            .ok_or_else(|| DomainError::database(format!("Type ID {type_id} not found")))?;

        self.load_full_type(db, &type_model).await
    }

    /// Load a full type from a model, resolving junction references.
    async fn load_full_type<C: DBRunner>(
        &self,
        db: &C,
        type_model: &gts_type::Model,
    ) -> Result<ResourceGroupType, DomainError> {
        let allowed_parent_types = Self::load_allowed_parent_types(db, type_model.id).await?;
        let allowed_membership_types =
            Self::load_allowed_membership_types(db, type_model.id).await?;

        // Derive can_be_root from stored metadata_schema internal key.
        // Per the placement invariant: can_be_root == true OR len(allowed_parent_types) >= 1.
        // If no allowed_parent_types, can_be_root must be true.
        let can_be_root = type_model
            .metadata_schema
            .as_ref()
            .and_then(|ms| ms.get("__can_be_root"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(allowed_parent_types.is_empty());

        // Extract the user-facing metadata_schema without internal keys.
        // Non-object schemas are stored under `__user_schema`; restore them on read.
        let metadata_schema = type_model.metadata_schema.as_ref().and_then(|ms| {
            if let serde_json::Value::Object(map) = ms {
                if let Some(user_schema) = map.get("__user_schema") {
                    return Some(user_schema.clone());
                }
                let filtered: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .filter(|(k, _)| !k.starts_with("__"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if filtered.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(filtered))
                }
            } else {
                Some(ms.clone())
            }
        });

        Ok(ResourceGroupType {
            code: type_model.schema_id.clone(),
            can_be_root,
            allowed_parent_types,
            allowed_membership_types,
            metadata_schema,
        })
    }

    /// Resolve a GTS type path to its surrogate SMALLINT ID.
    async fn resolve_id<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<i16>, DomainError> {
        Self::resolve_id(db, code).await
    }

    /// Insert a new GTS type. Returns the inserted model.
    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        schema_id: &str,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<gts_type::Model, DomainError> {
        let scope = system_scope();

        let model = gts_type::ActiveModel {
            schema_id: Set(schema_id.to_owned()),
            metadata_schema: Set(metadata_schema.cloned()),
            ..Default::default()
        };

        let _result = modkit_db::secure::secure_insert::<GtsTypeEntity>(model, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // Re-read to get the auto-generated ID
        Self::find_model_by_code(db, schema_id).await
    }

    /// Insert allowed parent junction entries.
    async fn insert_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        parent_ids: &[i16],
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        for &parent_id in parent_ids {
            let model = gts_type_allowed_parent::ActiveModel {
                type_id: Set(type_id),
                parent_type_id: Set(parent_id),
            };
            modkit_db::secure::secure_insert::<AllowedParentEntity>(model, &scope, db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }
        Ok(())
    }

    /// Insert allowed membership junction entries.
    async fn insert_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        membership_ids: &[i16],
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        for &membership_id in membership_ids {
            let model = gts_type_allowed_membership::ActiveModel {
                type_id: Set(type_id),
                membership_type_id: Set(membership_id),
            };
            modkit_db::secure::secure_insert::<AllowedMembershipEntity>(model, &scope, db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
        }
        Ok(())
    }

    /// Delete all allowed parent junction entries for a type.
    async fn delete_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        AllowedParentEntity::delete_many()
            .filter(gts_type_allowed_parent::Column::TypeId.eq(type_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Delete all allowed membership junction entries for a type.
    async fn delete_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        AllowedMembershipEntity::delete_many()
            .filter(gts_type_allowed_membership::Column::TypeId.eq(type_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Update the `gts_type` row (`metadata_schema`, `updated_at`).
    async fn update_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        code: &str,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<gts_type::Model, DomainError> {
        let scope = system_scope();

        // Use SecureUpdateMany for scoped update
        GtsTypeEntity::update_many()
            .filter(gts_type::Column::Id.eq(type_id))
            .secure()
            .col_expr(
                gts_type::Column::MetadataSchema,
                Expr::value(metadata_schema.cloned()),
            )
            .col_expr(
                gts_type::Column::UpdatedAt,
                Expr::value(time::OffsetDateTime::now_utc()),
            )
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Self::find_model_by_code(db, code).await
    }

    /// Delete a GTS type by its surrogate ID. CASCADE handles junction rows.
    async fn delete_by_id<C: DBRunner>(&self, db: &C, type_id: i16) -> Result<(), DomainError> {
        let scope = system_scope();
        GtsTypeEntity::delete_many()
            .filter(gts_type::Column::Id.eq(type_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(())
    }

    /// Count resource groups of a given type.
    async fn count_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<u64, DomainError> {
        let scope = system_scope();
        let count = ResourceGroupEntity::find()
            .filter(rg_entity::Column::GtsTypeId.eq(type_id))
            .secure()
            .scope_with(&scope)
            .count(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(count)
    }

    /// Find resource groups that have a specific parent type and are of a given child type.
    ///
    /// Uses a batch lookup instead of N+1 individual parent queries.
    async fn find_groups_using_parent_type<C: DBRunner>(
        &self,
        db: &C,
        child_type_id: i16,
        parent_type_id: i16,
    ) -> Result<Vec<(uuid::Uuid, String)>, DomainError> {
        let scope = system_scope();
        let groups: Vec<rg_entity::Model> = ResourceGroupEntity::find()
            .filter(rg_entity::Column::GtsTypeId.eq(child_type_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // Collect all parent IDs in a single batch
        let parent_ids: Vec<uuid::Uuid> = groups.iter().filter_map(|g| g.parent_id).collect();

        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Batch-load all parent groups in a single query
        let parents: Vec<rg_entity::Model> = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.is_in(parent_ids))
            .filter(rg_entity::Column::GtsTypeId.eq(parent_type_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let matching_parent_ids: std::collections::HashSet<uuid::Uuid> =
            parents.into_iter().map(|p| p.id).collect();

        // Filter child groups whose parent matched the target type
        let violations = groups
            .into_iter()
            .filter(|g| {
                g.parent_id
                    .is_some_and(|pid| matching_parent_ids.contains(&pid))
            })
            .map(|g| (g.id, g.name))
            .collect();

        Ok(violations)
    }

    /// Find root groups (`parent_id` IS NULL) of a given type.
    async fn find_root_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<Vec<(uuid::Uuid, String)>, DomainError> {
        let scope = system_scope();
        let groups: Vec<rg_entity::Model> = ResourceGroupEntity::find()
            .filter(rg_entity::Column::GtsTypeId.eq(type_id))
            .filter(Expr::col(rg_entity::Column::ParentId).is_null())
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(groups.into_iter().map(|g| (g.id, g.name)).collect())
    }

    /// List GTS types with `OData` filtering and cursor-based pagination.
    async fn list_types<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, DomainError> {
        let scope = system_scope();
        let base_query = GtsTypeEntity::find().secure().scope_with(&scope);

        let page = paginate_odata::<TypeFilterField, TypeODataMapper, _, _, _, _>(
            base_query,
            db,
            query,
            ("code", SortDir::Desc),
            TYPE_LIMIT_CFG,
            |m: gts_type::Model| m,
        )
        .await
        .map_err(|e| DomainError::database(e.to_string()))?;

        // Resolve full types (junction references) for each model in the page
        let mut types = Vec::with_capacity(page.items.len());
        for model in &page.items {
            let rg_type = self.load_full_type(db, model).await?;
            types.push(rg_type);
        }

        Ok(Page {
            items: types,
            page_info: page.page_info,
        })
    }

    /// Resolve multiple GTS type paths to their surrogate IDs.
    async fn resolve_ids<C: DBRunner>(
        &self,
        db: &C,
        codes: &[String],
    ) -> Result<Vec<i16>, DomainError> {
        if codes.is_empty() {
            return Ok(Vec::new());
        }

        let scope = system_scope();
        let types = GtsTypeEntity::find()
            .filter(gts_type::Column::SchemaId.is_in(codes.to_vec()))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let found_codes: Vec<&str> = types.iter().map(|t| t.schema_id.as_str()).collect();
        let missing: Vec<&str> = codes
            .iter()
            .filter(|c| !found_codes.contains(&c.as_str()))
            .map(String::as_str)
            .collect();

        if !missing.is_empty() {
            return Err(DomainError::validation(format!(
                "Referenced types not found: {}",
                missing.join(", ")
            )));
        }

        Ok(types.into_iter().map(|t| t.id).collect())
    }
}
