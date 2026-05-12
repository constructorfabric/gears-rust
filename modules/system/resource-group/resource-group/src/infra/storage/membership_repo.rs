// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-membership-service:p1
//! Persistence layer for membership management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths and UUIDs.

use async_trait::async_trait;
use modkit_db::odata::{LimitCfg, paginate_odata};
use modkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt};
use modkit_odata::{ODataQuery, Page, SortDir};
use modkit_security::AccessScope;
use resource_group_sdk::models::ResourceGroupMembership;
use resource_group_sdk::odata::MembershipFilterField;
use sea_orm::sea_query::SelectStatement;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, QueryTrait, Set};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::MembershipRepositoryTrait;
use crate::infra::storage::entity::resource_group::{
    self as rg_entity, Entity as ResourceGroupEntity,
};
use crate::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use crate::infra::storage::odata_mapper::MembershipODataMapper;

/// Default `OData` pagination limits for memberships.
const MEMBERSHIP_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// System-level access scope (no tenant/resource filtering).
fn system_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Build a sub-query that selects the IDs of every `resource_group` row the
/// caller's `AccessScope` covers.
///
/// `resource_group_membership` has no `tenant_id` column (see `DESIGN.md` §4.1),
/// so tenant filtering for memberships flows through this sub-query against
/// `resource_group`. Using `SecureORM`'s `scope_with` here means the resulting
/// WHERE clause supports every predicate kind the scope can carry —
/// `In(OWNER_TENANT_ID)`, `InTenantSubtree`, `InGroupSubtree`, etc. — not just
/// flat tenant lists.
fn allowed_group_ids_subquery(scope: &AccessScope) -> SelectStatement {
    ResourceGroupEntity::find()
        .secure()
        .scope_with(scope)
        .into_inner()
        .select_only()
        .column(rg_entity::Column::Id)
        .into_query()
}

/// Repository for membership persistence operations.
pub struct MembershipRepository;

#[async_trait]
impl MembershipRepositoryTrait for MembershipRepository {
    /// List memberships with `OData` filtering and pagination.
    ///
    /// The `OData` filter supports `group_id`, `resource_type`, and `resource_id` fields.
    /// `resource_type` values in filters are GTS type path strings; they are resolved
    /// to surrogate IDs at the persistence boundary.
    ///
    /// Tenant-scope is applied via a sub-query against `resource_group.tenant_id`
    /// because `resource_group_membership` has no `tenant_id` column.
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError> {
        // Defer all tenant filter shape (flat tenant list, subtree, etc.) to
        // SecureORM by building the allowed-group-id sub-query from the scope.
        let sub = allowed_group_ids_subquery(scope);
        let system = system_scope();
        let base_query = MembershipEntity::find()
            .filter(membership_entity::Column::GroupId.in_subquery(sub))
            .secure()
            .scope_with(&system);

        let page = paginate_odata::<MembershipFilterField, MembershipODataMapper, _, _, _, _>(
            base_query,
            db,
            query,
            ("group_id", SortDir::Desc),
            MEMBERSHIP_LIMIT_CFG,
            |m: membership_entity::Model| m,
        )
        .await
        .map_err(|e| DomainError::database(e.to_string()))?;

        // Batch-resolve type IDs to GTS paths (single query)
        let type_ids: Vec<i16> = page.items.iter().map(|m| m.gts_type_id).collect();
        let group_repo = crate::infra::storage::group_repo::GroupRepository;
        let type_map = crate::domain::repo::GroupRepositoryTrait::resolve_type_paths_batch(
            &group_repo,
            db,
            &type_ids,
        )
        .await?;

        let memberships = page
            .items
            .into_iter()
            .map(|model| {
                let type_path = type_map
                    .get(&model.gts_type_id)
                    .cloned()
                    .unwrap_or_default();
                ResourceGroupMembership {
                    group_id: model.group_id,
                    resource_type: type_path,
                    resource_id: model.resource_id,
                }
            })
            .collect();

        Ok(Page {
            items: memberships,
            page_info: page.page_info,
        })
    }

    /// Insert a membership. Returns the created membership with resolved type path.
    ///
    /// `scope` is threaded to the post-insert read-back so a cross-tenant
    /// insert surfaces as `MembershipNotFound` (defense-in-depth over the
    /// domain-layer `ensure_tenant_in_scope` check). The `secure_insert` call
    /// uses `system_scope` because `resource_group_membership` has no
    /// scopable columns — `secure_insert`'s property resolver would fail-closed
    /// on tenant constraints that don't map to any column.
    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<membership_entity::Model, DomainError> {
        let model = membership_entity::ActiveModel {
            group_id: Set(group_id),
            gts_type_id: Set(gts_type_id),
            resource_id: Set(resource_id.to_owned()),
            created_at: Set(time::OffsetDateTime::now_utc()),
        };

        modkit_db::secure::secure_insert::<MembershipEntity>(model, &system_scope(), db)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("duplicate key") || msg.contains("UNIQUE constraint") {
                    DomainError::duplicate_membership(format!(
                        "Membership already exists: ({group_id}, type_id={gts_type_id}, {resource_id})"
                    ))
                } else {
                    DomainError::database(msg)
                }
            })?;

        // Scope-aware read-back: a cross-tenant write surfaces as missing.
        self.find_by_composite_key(db, scope, group_id, gts_type_id, resource_id)
            .await?
            .ok_or_else(|| DomainError::database("Insert succeeded but membership not found"))
    }

    /// Delete a membership by its composite key. Returns the number of affected rows.
    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<u64, DomainError> {
        let allowed_groups = allowed_group_ids_subquery(scope);
        let system = system_scope();
        let result = MembershipEntity::delete_many()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .filter(membership_entity::Column::GroupId.in_subquery(allowed_groups))
            .secure()
            .scope_with(&system)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(result.rows_affected)
    }

    /// Find a membership by its composite key.
    async fn find_by_composite_key<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Option<membership_entity::Model>, DomainError> {
        let allowed_groups = allowed_group_ids_subquery(scope);
        let system = system_scope();
        MembershipEntity::find()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .filter(membership_entity::Column::GroupId.in_subquery(allowed_groups))
            .secure()
            .scope_with(&system)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// Check existing membership tenants for a resource (for tenant compatibility).
    /// Returns the set of distinct `tenant_ids` for groups that have this resource as a member.
    async fn get_existing_membership_tenant_ids<C: DBRunner>(
        &self,
        db: &C,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Vec<Uuid>, DomainError> {
        use crate::infra::storage::entity::resource_group::{
            self as rg_entity, Entity as ResourceGroupEntity,
        };

        let scope = system_scope();

        // Get all group_ids for this resource
        let memberships = MembershipEntity::find()
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        if memberships.is_empty() {
            return Ok(Vec::new());
        }

        let group_ids: Vec<Uuid> = memberships.iter().map(|m| m.group_id).collect();

        // Get tenant_ids from those groups
        let groups = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.is_in(group_ids))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        let mut tenant_ids: Vec<Uuid> = groups.into_iter().map(|g| g.tenant_id).collect();
        tenant_ids.sort();
        tenant_ids.dedup();
        Ok(tenant_ids)
    }
}
