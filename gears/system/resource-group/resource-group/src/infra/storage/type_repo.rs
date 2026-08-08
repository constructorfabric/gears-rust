// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1
//! Persistence layer for GTS type management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths.

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupType;
use resource_group_sdk::odata::TypeFilterField;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;

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
    /// Batch-resolve full types (junction references) for a whole page of
    /// models in a constant number of queries, regardless of page size.
    ///
    /// One query per junction table across the whole page, one combined
    /// id->code resolution query for every referenced parent/membership
    /// type, then assembles each `ResourceGroupType` in memory (RG-12).
    async fn load_full_types_batch(
        db: &impl DBRunner,
        models: &[gts_type::Model],
    ) -> Result<Vec<ResourceGroupType>, DomainError> {
        if models.is_empty() {
            return Ok(Vec::new());
        }
        let scope = system_scope();
        let type_ids: Vec<i16> = models.iter().map(|m| m.id).collect();

        let parent_rows = AllowedParentEntity::find()
            .filter(gts_type_allowed_parent::Column::TypeId.is_in(type_ids.clone()))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let membership_rows = AllowedMembershipEntity::find()
            .filter(gts_type_allowed_membership::Column::TypeId.is_in(type_ids))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let referenced_ids: std::collections::HashSet<i16> = parent_rows
            .iter()
            .map(|r| r.parent_type_id)
            .chain(membership_rows.iter().map(|r| r.membership_type_id))
            .collect();

        let code_by_id: std::collections::HashMap<i16, String> = if referenced_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            let ids: Vec<i16> = referenced_ids.into_iter().collect();
            GtsTypeEntity::find()
                .filter(gts_type::Column::Id.is_in(ids))
                .secure()
                .scope_with(&scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?
                .into_iter()
                .map(|m| (m.id, m.schema_id))
                .collect()
        };

        let mut parents_by_type: std::collections::HashMap<i16, Vec<String>> =
            std::collections::HashMap::new();
        for r in &parent_rows {
            if let Some(code) = code_by_id.get(&r.parent_type_id) {
                parents_by_type
                    .entry(r.type_id)
                    .or_default()
                    .push(code.clone());
            }
        }
        let mut memberships_by_type: std::collections::HashMap<i16, Vec<String>> =
            std::collections::HashMap::new();
        for r in &membership_rows {
            if let Some(code) = code_by_id.get(&r.membership_type_id) {
                memberships_by_type
                    .entry(r.type_id)
                    .or_default()
                    .push(code.clone());
            }
        }

        Ok(models
            .iter()
            .map(|m| {
                let mut allowed_parent_types = parents_by_type.remove(&m.id).unwrap_or_default();
                allowed_parent_types.sort();
                let mut allowed_membership_types =
                    memberships_by_type.remove(&m.id).unwrap_or_default();
                allowed_membership_types.sort();
                Self::build_resource_group_type(m, allowed_parent_types, allowed_membership_types)
            })
            .collect())
    }

    /// Assemble a `ResourceGroupType` from a raw model plus resolved junction
    /// data. Shared by `load_full_type` and `load_full_types_batch` so both
    /// derive `can_be_root`/`metadata_schema` identically.
    fn build_resource_group_type(
        type_model: &gts_type::Model,
        allowed_parent_types: Vec<String>,
        allowed_membership_types: Vec<String>,
    ) -> ResourceGroupType {
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

        ResourceGroupType {
            code: type_model.schema_id.clone(),
            can_be_root,
            allowed_parent_types,
            allowed_membership_types,
            metadata_schema,
        }
    }

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

    /// Same lookup as `find_by_code`, but also returns the surrogate
    /// SMALLINT id, for callers that need both (e.g. a group's
    /// `gts_type_id` FK) from a single query instead of two (RG-11).
    async fn find_by_code_with_id<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<(gts_type::Model, ResourceGroupType)>, DomainError> {
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

        self.load_full_type(db, &type_model)
            .await
            .map(|rg_type| Some((type_model, rg_type)))
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

        Ok(Self::build_resource_group_type(
            type_model,
            allowed_parent_types,
            allowed_membership_types,
        ))
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

        // Use the row the insert already returned. Discarding it and
        // re-reading by code cost a second `gts_type` SELECT on every
        // create (RG-08); `secure_insert` returns the persisted model with
        // the generated id in it.
        toolkit_db::secure::secure_insert::<GtsTypeEntity>(model, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// Insert allowed parent junction entries.
    async fn insert_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        parent_ids: &[i16],
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        // One INSERT for the whole set: the row count is the size of the
        // request, and a per-row loop holds the enclosing transaction open
        // proportionally longer for no gain.
        let rows: Vec<gts_type_allowed_parent::ActiveModel> = parent_ids
            .iter()
            .map(|&parent_id| gts_type_allowed_parent::ActiveModel {
                type_id: Set(type_id),
                parent_type_id: Set(parent_id),
            })
            .collect();
        toolkit_db::secure::secure_insert_many::<AllowedParentEntity>(rows, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
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
        // One INSERT for the whole set -- see `insert_allowed_parent_types`.
        let rows: Vec<gts_type_allowed_membership::ActiveModel> = membership_ids
            .iter()
            .map(|&membership_id| gts_type_allowed_membership::ActiveModel {
                type_id: Set(type_id),
                membership_type_id: Set(membership_id),
            })
            .collect();
        toolkit_db::secure::secure_insert_many::<AllowedMembershipEntity>(rows, &scope, db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
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
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<time::OffsetDateTime, DomainError> {
        let scope = system_scope();
        let updated_at = time::OffsetDateTime::now_utc();

        // Use SecureUpdateMany for scoped update
        GtsTypeEntity::update_many()
            .filter(gts_type::Column::Id.eq(type_id))
            .secure()
            .col_expr(
                gts_type::Column::MetadataSchema,
                Expr::value(metadata_schema.cloned()),
            )
            .col_expr(gts_type::Column::UpdatedAt, Expr::value(updated_at))
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // The timestamp is handed back rather than read back: it is the only
        // thing this write decided that the caller did not already know.
        Ok(updated_at)
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

    /// Batch replacement for a removed-allowed-parent-types sweep -- see the
    /// trait doc comment for the full rationale (N+1 audit finding (b)).
    ///
    /// Three queries regardless of how many `parent_codes` are checked:
    /// resolve every candidate path to its surrogate id, load every group
    /// of `child_type_id` once, then batch-match those groups' actual
    /// parents against every candidate parent-type id at once.
    async fn find_groups_violating_removed_parents<C: DBRunner>(
        &self,
        db: &C,
        child_type_id: i16,
        parent_codes: &[String],
    ) -> Result<Vec<(String, uuid::Uuid, String)>, DomainError> {
        if parent_codes.is_empty() {
            return Ok(Vec::new());
        }
        let scope = system_scope();

        // Resolve every candidate parent-type path in one query. A path
        // that doesn't currently resolve to any gts_type row is silently
        // dropped -- no group can reference a type that no longer exists.
        //
        // `parent_codes` is `allowed_parent_types` straight off the request
        // body, so its length is the client's choice, not the schema's:
        // chunked against the bind ceiling like every other list-valued
        // predicate here.
        let mut parent_types: Vec<gts_type::Model> = Vec::new();
        for chunk in parent_codes.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            let found = GtsTypeEntity::find()
                .filter(gts_type::Column::SchemaId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
            parent_types.extend(found);
        }

        if parent_types.is_empty() {
            return Ok(Vec::new());
        }

        let code_by_id: std::collections::HashMap<i16, String> = parent_types
            .iter()
            .map(|t| (t.id, t.schema_id.clone()))
            .collect();
        let parent_type_ids: Vec<i16> = parent_types.iter().map(|t| t.id).collect();

        // Groups of the child type being updated -- one query, independent
        // of how many candidate parent types there are.
        let groups: Vec<rg_entity::Model> = ResourceGroupEntity::find()
            .filter(rg_entity::Column::GtsTypeId.eq(child_type_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        // One entry per group that has a parent, so this list grows with the
        // number of groups of the child type, not with the number of
        // candidate codes -- and siblings sharing a parent repeat it.
        // Deduplicate before binding: it is both fewer parameters and fewer
        // rows for the database to match.
        let mut parent_ids: Vec<uuid::Uuid> = groups.iter().filter_map(|g| g.parent_id).collect();
        parent_ids.sort_unstable();
        parent_ids.dedup();
        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Which of those groups' actual parents are of one of the
        // candidate parent types -- one query for every candidate at once,
        // not one per candidate.
        //
        // Chunked against the backend's bind-parameter ceiling for the same
        // reason the batch deletes in `group_repo` are: an unbounded
        // `IN (...)` fails in the driver rather than in the query.
        //
        // Two predicates share one budget, and *neither* list is bounded by
        // construction -- `parent_codes` comes from the request body. So both
        // are chunked, and a chunk of each has to fit together. Subtracting
        // the type ids off the top is not enough: if that list alone exceeds
        // the ceiling, every query still binds it whole and still overruns.
        //
        // Capping the type ids at half leaves the ids at least the other
        // half, so the inner budget is never squeezed to nothing. In the
        // ordinary case -- a handful of candidate parent types -- the outer
        // loop runs once and the ids get the whole remainder.
        let budget = toolkit_db::secure::max_bind_params_for(db);
        let type_budget = budget.div_euclid(2).max(1);

        let mut parent_type_by_id: std::collections::HashMap<uuid::Uuid, i16> =
            std::collections::HashMap::new();
        for type_chunk in parent_type_ids.chunks(type_budget) {
            let id_budget = budget.saturating_sub(type_chunk.len()).max(1);
            for id_chunk in parent_ids.chunks(id_budget) {
                let parents: Vec<rg_entity::Model> = ResourceGroupEntity::find()
                    .filter(rg_entity::Column::Id.is_in(id_chunk.to_vec()))
                    .filter(rg_entity::Column::GtsTypeId.is_in(type_chunk.to_vec()))
                    .secure()
                    .scope_with(&scope)
                    .all(db)
                    .await
                    .map_err(|e| DomainError::database(e.to_string()))?;

                parent_type_by_id.extend(parents.into_iter().map(|p| (p.id, p.gts_type_id)));
            }
        }

        Ok(groups
            .into_iter()
            .filter_map(|g| {
                let pid = g.parent_id?;
                let parent_type_id = *parent_type_by_id.get(&pid)?;
                let code = code_by_id.get(&parent_type_id)?.clone();
                Some((code, g.id, g.name))
            })
            .collect())
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

        // Resolve the junction references for the whole page in a constant
        // number of queries: one per junction table plus one combined
        // id->code resolution, instead of two per row (RG-12).
        let types = Self::load_full_types_batch(db, &page.items).await?;

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

        // `codes` is `allowed_parent_types` / `allowed_membership_types`
        // straight off the request body -- its length is the client's choice.
        // Chunked against the bind ceiling for the same reason as everywhere
        // else: an unbounded `IN (...)` fails in the driver, not in the query.
        let mut types: Vec<gts_type::Model> = Vec::new();
        for chunk in codes.chunks(toolkit_db::secure::max_bind_params_for(db)) {
            let found = GtsTypeEntity::find()
                .filter(gts_type::Column::SchemaId.is_in(chunk.to_vec()))
                .secure()
                .scope_with(&scope)
                .all(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?;
            types.extend(found);
        }

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
