// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-membership-service:p1
//! Persistence layer for membership management.
//!
//! All surrogate SMALLINT ID resolution happens here. The domain and API layers
//! work exclusively with string GTS type paths and UUIDs.

use async_trait::async_trait;
use resource_group_sdk::models::ResourceGroupMembership;
use resource_group_sdk::odata::MembershipFilterField;
use sea_orm::ExprTrait;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::MembershipRepositoryTrait;
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

/// Repository for membership persistence operations.
pub struct MembershipRepository;

#[async_trait]
impl MembershipRepositoryTrait for MembershipRepository {
    /// List memberships with `OData` filtering and pagination.
    ///
    /// The `OData` filter supports `group_id`, `resource_type`, and `resource_id` fields.
    /// `resource_type` values in filters are GTS type path strings; they are resolved
    /// to surrogate IDs at the persistence boundary.
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError> {
        let scope = system_scope();
        let base_query = MembershipEntity::find().secure().scope_with(&scope);

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
    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<membership_entity::Model, DomainError> {
        let scope = system_scope();

        let created_at = time::OffsetDateTime::now_utc();

        let model = membership_entity::ActiveModel {
            group_id: Set(group_id),
            gts_type_id: Set(gts_type_id),
            resource_id: Set(resource_id.to_owned()),
            created_at: Set(created_at),
        };

        toolkit_db::secure::secure_insert::<MembershipEntity>(model, &scope, db)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    DomainError::duplicate_membership(
                        format!("({group_id}, type_id={gts_type_id}, {resource_id})"),
                        format!(
                            "Membership already exists: ({group_id}, type_id={gts_type_id}, {resource_id})"
                        ),
                    )
                } else {
                    DomainError::database(e.to_string())
                }
            })?;

        // Assembled, not read back (RG-08). This table has four columns:
        // three primary-key parts and `created_at`, and all four were just
        // written from values held right here. The row the database now holds
        // is fully determined, so the read this replaces could only return
        // what is already in scope.
        Ok(membership_entity::Model {
            group_id,
            gts_type_id,
            resource_id: resource_id.to_owned(),
            created_at,
        })
    }

    /// Delete a membership by its composite key. Returns the number of affected rows.
    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<u64, DomainError> {
        let scope = system_scope();
        let result = MembershipEntity::delete_many()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .secure()
            .scope_with(&scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(result.rows_affected)
    }

    /// Find a membership by its composite key.
    async fn find_by_composite_key<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Option<membership_entity::Model>, DomainError> {
        let scope = system_scope();
        MembershipEntity::find()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .secure()
            .scope_with(&scope)
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

        // The group ids feeding the second query were exactly the first
        // query's result, so the database can derive them: one statement with
        // two bind parameters, not two statements the second of which bound
        // one parameter per membership of the resource -- unbounded, and
        // unchunked.
        //
        // The inner subquery over `resource_group_membership` is
        // deliberately hand-built and unscoped, not an oversight: this is an
        // integrity read (cross-tenant compatibility) and must see every
        // membership row regardless of scope. `resource_group_membership`
        // declares no scope columns, so a constrained `AccessScope` run
        // through `build_scope_condition` against it does not degrade to
        // "no filter" -- every constraint fails to resolve a column and the
        // whole condition compiles to `WHERE false` (`cond.rs`,
        // `build_constraint_condition`'s early return through
        // `resolve_property`). Scoping this subquery under a constrained
        // scope would make it silently see zero memberships and report the
        // resource tenant-compatible with everything. This method builds
        // `system_scope()` itself, precisely so no constrained scope can
        // reach here; if it ever takes a caller-supplied scope instead,
        // do not scope this subquery -- rethink what "existing membership
        // tenants" is even supposed to mean under a partial view first.
        let member_group_ids = Query::select()
            .column(membership_entity::Column::GroupId)
            .from(MembershipEntity)
            .and_where(Expr::col(membership_entity::Column::GtsTypeId).eq(gts_type_id))
            .and_where(Expr::col(membership_entity::Column::ResourceId).eq(resource_id))
            .to_owned();

        let groups = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.in_subquery(member_group_ids))
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

    async fn ensure_membership_guard<C: DBRunner>(
        &self,
        db: &C,
        gts_type_id: i16,
        resource_id: &str,
        tenant_id: Uuid,
    ) -> Result<Uuid, DomainError> {
        use crate::infra::storage::entity::resource_membership_tenant::{
            self as guard_entity, Entity as GuardEntity,
        };

        let sc = system_scope();

        // Optimistic: try to read first; if none exists, insert.
        let existing = GuardEntity::find()
            .filter(guard_entity::Column::GtsTypeId.eq(gts_type_id))
            .filter(guard_entity::Column::ResourceId.eq(resource_id))
            .secure()
            .scope_with(&sc)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        if let Some(guard) = existing {
            return Ok(guard.tenant_id);
        }

        // No guard row yet — try to claim this resource.
        let model = guard_entity::ActiveModel {
            gts_type_id: Set(gts_type_id),
            resource_id: Set(resource_id.to_owned()),
            tenant_id: Set(tenant_id),
            created_at: Set(time::OffsetDateTime::now_utc()),
        };

        let result = toolkit_db::secure::secure_insert::<GuardEntity>(model, &sc, db).await;
        if result.is_ok() {
            return Ok(tenant_id);
        }
        let err = result.unwrap_err();
        if err.is_unique_violation() {
            // Lost the race. Read the established tenant.
            let winner = GuardEntity::find()
                .filter(guard_entity::Column::GtsTypeId.eq(gts_type_id))
                .filter(guard_entity::Column::ResourceId.eq(resource_id))
                .secure()
                .scope_with(&sc)
                .one(db)
                .await
                .map_err(|e| DomainError::database(e.to_string()))?
                .ok_or_else(|| {
                    DomainError::database(
                        "Guard row disappeared after UNIQUE violation".to_owned(),
                    )
                })?;
            Ok(winner.tenant_id)
        } else {
            Err(DomainError::database(err.to_string()))
        }
    }
}
