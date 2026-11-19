// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-membership-service:p1
//! Persistence layer for membership management.
//!
//! Memberships store the `SMALLINT` key of the member's GTS type in RG's local
//! identity dictionary (`gts_type`, see `gts_type_dict`). Keys are resolved to
//! GTS type identifiers here, batched per page; the domain and API layers work
//! with string GTS type identifiers and UUIDs only.

use async_trait::async_trait;
use resource_group_sdk::models::ResourceGroupMembership;
use resource_group_sdk::odata::MembershipFilterField;
use sea_orm::ExprTrait;
use sea_orm::sea_query::{Expr, Query, SelectStatement};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, QueryTrait, Set};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, DbTx, SecureDeleteExt, SecureEntityExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::odata_filter::prepare_filter;
use crate::domain::error::DomainError;
use crate::domain::membership_service::group_scope_for_membership;
use crate::domain::repo::{GtsTypeKey, MemberTypes, MembershipRepositoryTrait};
use crate::infra::storage::FK_RGM_GROUP_ID;
use crate::infra::storage::entity::resource_group::{
    self as rg_entity, Entity as ResourceGroupEntity,
};
use crate::infra::storage::entity::resource_group_membership::{
    self as membership_entity, Entity as MembershipEntity,
};
use crate::infra::storage::gts_type_dict;
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

/// Ids of the groups visible under `scope`, as a subquery.
///
/// The scope was issued for the membership resource type and is compiled
/// against the group entity here. A property other than the owner tenant id
/// would be read in terms of the group (a resource id would be read as the
/// group id), so the scope is first narrowed by `group_scope_for_membership`,
/// which fails closed: such a scope matches no group.
fn groups_in_scope(scope: &AccessScope) -> SelectStatement {
    ResourceGroupEntity::find()
        .secure()
        .scope_with(&group_scope_for_membership(scope))
        .into_inner()
        .select_only()
        .column(rg_entity::Column::Id)
        .into_query()
}

/// Membership-side condition that restricts rows to groups visible under
/// `scope`. `None` for an unconstrained scope: no tenant filtering, and no
/// semi-join over all groups. A deny-all scope, and any scope that fails
/// closed in `group_scope_for_membership`, yields a subquery with
/// `WHERE false`, so no rows match.
fn membership_group_condition(scope: &AccessScope) -> Option<Expr> {
    if scope.is_unconstrained() {
        return None;
    }
    Some(membership_entity::Column::GroupId.in_subquery(groups_in_scope(scope)))
}

/// Repository for membership persistence operations.
pub struct MembershipRepository;

#[async_trait]
impl MembershipRepositoryTrait for MembershipRepository {
    /// List memberships with `OData` filtering and pagination.
    ///
    /// The `OData` filter supports `group_id`, `resource_type`, and `resource_id` fields.
    /// `resource_type` takes GTS type identifiers and is matched through the
    /// dictionary in SQL; no type registry is consulted.
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError> {
        // The membership table has no scope columns, so the row itself is
        // read under the system scope. The caller's tenant scope is applied
        // through the owning group (see `membership_group_condition`).
        let row_scope = system_scope();
        let base_query = MembershipEntity::find()
            .apply_if(membership_group_condition(scope), QueryFilter::filter)
            .secure()
            .scope_with(&row_scope);
        let (base_query, query_no_filter) =
            prepare_filter::<MembershipFilterField, MembershipODataMapper, _>(
                base_query,
                query,
                MembershipFilterField::ResourceType,
                toolkit_db::secure::max_bind_params_for(db),
            )?;

        let page = paginate_odata::<MembershipFilterField, MembershipODataMapper, _, _, _, _>(
            base_query,
            db,
            &query_no_filter,
            ("group_id", SortDir::Desc),
            MEMBERSHIP_LIMIT_CFG,
            |m: membership_entity::Model| m,
        )
        .await
        .map_err(DomainError::from)?;

        let type_paths =
            gts_type_dict::find_paths(db, page.items.iter().map(|m| GtsTypeKey(m.gts_type_id)))
                .await?;
        let memberships = page
            .items
            .into_iter()
            .map(|model| {
                Ok(ResourceGroupMembership {
                    group_id: model.group_id,
                    resource_type: type_paths.code(GtsTypeKey(model.gts_type_id))?,
                    resource_id: model.resource_id,
                })
            })
            .collect::<Result<_, DomainError>>()?;

        Ok(Page {
            items: memberships,
            page_info: page.page_info,
        })
    }

    /// Insert a membership, keying its member type first.
    async fn insert(
        &self,
        db: &DbTx<'_>,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), DomainError> {
        let scope = system_scope();
        let type_key = gts_type_dict::ensure_key(db, resource_type).await?;

        let created_at = time::OffsetDateTime::now_utc();

        let model = membership_entity::ActiveModel {
            group_id: Set(group_id),
            gts_type_id: Set(type_key.0),
            resource_id: Set(resource_id.to_owned()),
            created_at: Set(created_at),
        };

        toolkit_db::secure::secure_insert::<MembershipEntity>(model, &scope, db)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    let key = format!("({group_id}, {resource_type}, {resource_id})");
                    DomainError::duplicate_membership(
                        key.clone(),
                        format!("Membership already exists: {key}"),
                    )
                } else if e.is_foreign_key_violation() {
                    // The group can be deleted between the caller's read of
                    // it and this insert, and then the answer comes from
                    // `fk_rgm_group_id` -- a missing group, not an internal
                    // failure. Matched by constraint name, the same way
                    // `GroupRepository::insert` does. PostgreSQL names the
                    // constraint; SQLite says
                    // only "FOREIGN KEY constraint failed", so there the
                    // answer stays a database error rather than a confident
                    // lie about which resource was missing.
                    let msg = e.to_string();
                    if msg.contains(FK_RGM_GROUP_ID) {
                        DomainError::group_not_found(group_id)
                    } else {
                        DomainError::database(msg)
                    }
                } else {
                    DomainError::database(e.to_string())
                }
            })?;

        // Nothing is read back (RG-08): every column was just written from
        // values the caller holds.
        Ok(())
    }

    /// Delete matching membership keys. Returns the number of affected rows.
    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        types: MemberTypes<'_>,
        resource_id: &str,
    ) -> Result<u64, DomainError> {
        // No scope columns on the membership row: tenant scope is applied
        // through the owning group, the row is run under the system scope.
        // The type is matched through the dictionary in the same statement,
        // never creating a key: an unknown type deletes nothing.
        let keys = match types {
            MemberTypes::Exact(code) => gts_type_dict::keys_subquery([code]),
            MemberTypes::Family { root, exact_alias } => {
                gts_type_dict::family_keys(root.as_ref(), exact_alias)
            }
        };
        let row_scope = system_scope();
        let result = MembershipEntity::delete_many()
            .filter(membership_entity::Column::GroupId.eq(group_id))
            .filter(membership_entity::Column::GtsTypeId.in_subquery(keys))
            .filter(membership_entity::Column::ResourceId.eq(resource_id))
            .apply_if(membership_group_condition(scope), QueryFilter::filter)
            .secure()
            .scope_with(&row_scope)
            .exec(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;
        Ok(result.rows_affected)
    }

    /// Whether the resource already belongs to a tenant other than
    /// `tenant_id`. See the trait doc for the isolation contract this read
    /// depends on (RG-01).
    async fn has_membership_in_other_tenant<C: DBRunner>(
        &self,
        db: &C,
        resource_type: &str,
        resource_id: &str,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = system_scope();

        // One statement, two bind parameters: the member group ids are
        // derived by the database rather than round-tripped through the
        // process, so nothing here grows with the number of memberships the
        // resource has.
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
        // do not scope this subquery -- rethink what "already owned by
        // another tenant" is even supposed to mean under a partial view
        // first.
        let member_group_ids = Query::select()
            .column(membership_entity::Column::GroupId)
            .from(MembershipEntity)
            .and_where(
                Expr::col(membership_entity::Column::GtsTypeId)
                    .in_subquery(gts_type_dict::keys_subquery([resource_type])),
            )
            .and_where(Expr::col(membership_entity::Column::ResourceId).eq(resource_id))
            .to_owned();

        // `LIMIT 1` and the tenant predicate pushed into the same statement:
        // the first foreign-tenant row settles the question, so this neither
        // counts nor materializes the rest.
        let conflicting = ResourceGroupEntity::find()
            .filter(rg_entity::Column::Id.in_subquery(member_group_ids))
            .filter(rg_entity::Column::TenantId.ne(tenant_id))
            .limit(1)
            .secure()
            .scope_with(&scope)
            .one(db)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(conflicting.is_some())
    }
}
