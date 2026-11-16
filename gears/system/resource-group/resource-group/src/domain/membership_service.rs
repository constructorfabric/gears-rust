// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-membership-service:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-membership:p1
//! Domain service for resource group membership management.
//!
//! Implements business rules for adding, removing, and listing memberships
//! between resources and groups. Delegates persistence to the infra layer.

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
use resource_group_sdk::{GROUP_MEMBERSHIP_RESOURCE_TYPE, models::ResourceGroupMembership};
use toolkit_db::secure::TxConfig;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use tracing::debug;

use crate::domain::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait, TypeRepositoryTrait};

/// `AuthZ` resource type descriptor for group memberships.
pub const RG_MEMBERSHIP_RESOURCE: ResourceType = ResourceType::from_static(
    GROUP_MEMBERSHIP_RESOURCE_TYPE,
    &[pep_properties::OWNER_TENANT_ID],
);

/// Narrow a scope issued for the membership resource type so it can be
/// applied to the owning group.
///
/// The scope is issued for `RG_MEMBERSHIP_RESOURCE` and applied to the group
/// that owns the membership. A property other than the owner tenant id would
/// be read in terms of the group (a resource id would be read as the group
/// id), so a scope that carries one grants nothing: the whole scope is closed,
/// not just the constraint that carries it. Fail closed.
#[must_use]
pub(crate) fn group_scope_for_membership(scope: &AccessScope) -> AccessScope {
    if scope.is_unconstrained() {
        return AccessScope::allow_all();
    }
    let foreign_property = scope.constraints().iter().any(|c| {
        c.filters()
            .iter()
            .any(|f| f.property() != pep_properties::OWNER_TENANT_ID)
    });
    if foreign_property {
        AccessScope::deny_all()
    } else {
        AccessScope::from_constraints(scope.constraints().to_vec())
    }
}

// @cpt-flow:cpt-cf-resource-group-flow-membership-add:p1
// @cpt-flow:cpt-cf-resource-group-flow-membership-remove:p1
// @cpt-flow:cpt-cf-resource-group-flow-membership-list:p1
// @cpt-dod:cpt-cf-resource-group-dod-membership-service:p1

/// Service for resource group membership lifecycle management.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
pub struct MembershipService<
    GR: GroupRepositoryTrait,
    TR: TypeRepositoryTrait,
    MR: MembershipRepositoryTrait,
> {
    db: Arc<DbProvider>,
    enforcer: PolicyEnforcer,
    group_repo: Arc<GR>,
    type_repo: Arc<TR>,
    membership_repo: Arc<MR>,
}

impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    MembershipService<GR, TR, MR>
{
    /// Create a new `MembershipService` with the given database provider
    /// and `PolicyEnforcer` for AuthZ-scoped queries.
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        enforcer: PolicyEnforcer,
        group_repo: Arc<GR>,
        type_repo: Arc<TR>,
        membership_repo: Arc<MR>,
    ) -> Self {
        Self {
            db,
            enforcer,
            group_repo,
            type_repo,
            membership_repo,
        }
    }

    fn conn(&self) -> Result<impl toolkit_db::secure::DBRunner + '_, DomainError> {
        self.db
            .conn()
            .map_err(|e| DomainError::database(e.to_string()))
    }

    /// Add a membership link between a resource and a group.
    ///
    /// Validates group existence, `resource_type` registration, `allowed_membership_types`
    /// compatibility, and tenant scope before inserting the membership row.
    /// The caller's scope is enforced through the owning group: a group
    /// outside the scope is reported as not found.
    pub async fn add_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-2
        // Validate resource_type is a valid GtsTypePath (validated implicitly by resolve)
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-2

        // AuthZ gate: verify the caller can create memberships
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_MEMBERSHIP_RESOURCE, "create", None)
            .await
            .map_err(DomainError::from)?;

        self.add_membership_inner(&scope, group_id, resource_type, resource_id)
            .await
    }

    /// Add a membership link without `AuthZ` enforcement.
    ///
    /// **Internal API** — never expose this through a REST handler. Used by
    /// the membership seeding adapter (which runs at gear init, before
    /// any caller `SecurityContext` exists). Domain invariants
    /// (group existence, type registration, `allowed_membership_types`
    /// compatibility, tenant scope) still run; only the `PolicyEnforcer`
    /// gate is skipped.
    pub async fn add_membership_unscoped(
        &self,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, DomainError> {
        self.add_membership_inner(
            &AccessScope::allow_all(),
            group_id,
            resource_type,
            resource_id,
        )
        .await
    }

    /// Shared post-authz body of `add_membership` / `add_membership_unscoped`.
    async fn add_membership_inner(
        &self,
        scope: &AccessScope,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, DomainError> {
        // Tenant compatibility, the allowed_membership_types check, and the
        // membership insert all share one transaction, and that transaction
        // is `SERIALIZABLE` (RG-01).
        //
        // The check reads a predicate -- "which tenants already own
        // memberships of this pair" -- and the insert then writes into that
        // same predicate. Run apart, as they were, two first memberships from
        // different tenants each read an empty set and both commit, and the
        // resource ends up owned by two tenants. Run together at the backend
        // default, each still reads from its own snapshot and neither sees
        // the other's uncommitted row: `READ COMMITTED` has nothing to say
        // about rows that did not exist when the statement began.
        //
        // At `SERIALIZABLE` that shape is write skew, which is precisely what
        // PostgreSQL's SSI cancels: the two read/write pairs form a cycle of
        // rw-antidependencies, one side is cancelled as the pivot with
        // `40001`, and the retry already wrapping this transaction re-runs
        // it against the committed winner and rejects. On SQLite writes
        // serialize regardless, so the same conclusion holds there for a
        // different reason.
        let db = self.db.db();
        let group_repo = self.group_repo.clone();
        let membership_repo = self.membership_repo.clone();
        let type_repo = self.type_repo.clone();
        let group_scope = group_scope_for_membership(scope);
        let resource_type_owned = resource_type.to_owned();
        let resource_id_owned = resource_id.to_owned();

        let model = db
            .transaction_with_retry(TxConfig::serializable(), DomainError::db_err, |tx| {
                let group_repo = group_repo.clone();
                let membership_repo = membership_repo.clone();
                let type_repo = type_repo.clone();
                let group_scope = group_scope.clone();
                let resource_type = resource_type_owned.clone();
                let resource_id = resource_id_owned.clone();
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-3
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-4
                    // Verify the group exists and is visible under the caller's
                    // scope, and get its type info. A group outside the scope
                    // is reported exactly like a missing one, so this is not an
                    // existence oracle. The read runs inside the same
                    // SERIALIZABLE transaction as the insert, so a group that
                    // was deleted and re-created with the same id under another
                    // tenant between the check and the insert cannot pass the
                    // gate.
                    let group_model = group_repo
                        .find_model_by_id_scoped(tx, &group_scope, group_id)
                        .await?
                        .ok_or(DomainError::GroupNotFound { id: group_id })?;
                    let target_tenant_id = group_model.tenant_id;
                    let group_type_id = group_model.gts_type_id;
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-4
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-3

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
                    // Resolve the GTS type path to a surrogate SMALLINT ID
                    let gts_type_id = type_repo
                        .resolve_id(tx, &resource_type)
                        .await?
                        .ok_or_else(|| {
                            DomainError::validation(format!(
                                "Unknown resource type: {resource_type}"
                            ))
                        })?;
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7
                    // Load group type's allowed_membership_types and validate.
                    //
                    // Moved inside this SERIALIZABLE transaction on purpose:
                    // PostgreSQL's SSI only tracks rw-antidependencies between
                    // reads and writes that both happen inside a serializable
                    // transaction. `update_type` (which can remove this
                    // resource type from `allowed_membership_types`) also runs
                    // at `SERIALIZABLE`, but a read of the type made on the
                    // pool -- outside any transaction -- is invisible to that
                    // machinery. Such a read could see the type as still
                    // allowing this membership, have `update_type` commit its
                    // removal in the gap, and then have this function insert
                    // the now-disallowed membership anyway, with neither side
                    // ever seeing a `40001`. Reading it here, inside the same
                    // transaction as the insert it gates, closes that gap.
                    let allowed = type_repo.load_full_type_by_id(tx, group_type_id).await?;
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8
                    if !allowed
                        .allowed_membership_types
                        .iter()
                        .any(|m| m == &resource_type)
                    {
                        return Err(DomainError::validation(format!(
                            "Resource type '{resource_type}' is not in allowed_membership_types for group type '{}'",
                            allowed.code
                        )));
                    }
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8

                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-1
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-2
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-3
                    // DB: does this resource already have a membership owned
                    // by another tenant? An existence check with `LIMIT 1`,
                    // not a count and not a scan of the memberships
                    // themselves. No memberships at all, or only this
                    // tenant's, and the answer is "no conflict" -- the first
                    // membership of a resource may come from any tenant.
                    let owned_elsewhere = membership_repo
                        .has_membership_in_other_tenant(
                            tx,
                            gts_type_id,
                            &resource_id,
                            target_tenant_id,
                        )
                        .await?;
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-3
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-2
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-1

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-9
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-10
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-4
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-5
                    if owned_elsewhere {
                        debug!(
                            group_id = %group_id,
                            resource_type = %resource_type,
                            resource_id = %resource_id,
                            "Tenant incompatibility on membership add"
                        );
                        // The message stays generic about *which* tenant owns
                        // the resource: that id belongs to a tenant other than
                        // the caller's, and this error reaches the caller
                        // verbatim over the API (api/rest/error.rs), so it
                        // must not name it.
                        return Err(DomainError::tenant_incompatibility(format!(
                            "Resource ({resource_type}, {resource_id}) is already \
                             linked to a different tenant"
                        )));
                    }
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-5
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-4
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-10
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-9

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-11
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-12
                    // Insert the membership (repo handles duplicate detection)
                    membership_repo
                        .insert(tx, group_id, gts_type_id, &resource_id)
                        .await
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-12
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-11
                })
            })
            .await?;

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-13
        // Resolve back to GTS path for the SDK model
        Ok(ResourceGroupMembership {
            group_id: model.group_id,
            resource_type: resource_type.to_owned(),
            resource_id: model.resource_id,
        })
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-13
    }

    /// Remove a membership link.
    ///
    /// Resolves the GTS type path and deletes the membership in one statement;
    /// zero affected rows is reported as not found.
    /// The caller's scope is enforced through the owning group: a membership
    /// in a group outside the scope is reported as not found, exactly like a
    /// missing one.
    pub async fn remove_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-1
        // Actor sends DELETE /api/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}
        // AuthZ gate: verify the caller can delete memberships
        let scope = self
            .enforcer
            .access_scope(ctx, &RG_MEMBERSHIP_RESOURCE, "delete", None)
            .await
            .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-1

        let conn = self.conn()?;

        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-2
        // Resolve resource_type GTS path to surrogate ID
        let gts_type_id = self
            .type_repo
            .resolve_id(&conn, resource_type)
            .await?
            .ok_or_else(|| {
                DomainError::validation(format!("Unknown resource type: {resource_type}"))
            })?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-2

        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-3
        // Delete the membership. There is no prior read: this is a single
        // statement by primary key, restricted to groups visible under the
        // caller's scope. Nothing is decided from a read, and with tenant
        // ownership derived from the memberships themselves there is no
        // second piece of state to keep in step, so this needs neither its
        // own transaction nor a level above the backend default. A
        // concurrent `add_membership` either sees this row (its own
        // `SERIALIZABLE` read, before this delete commits) and rejects, or
        // does not (after) and is the resource's first membership again --
        // both outcomes correct.
        let deleted = self
            .membership_repo
            .delete(&conn, &scope, group_id, gts_type_id, resource_id)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-3

        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-4
        // Zero affected rows covers both a missing membership and one in a
        // group outside the caller's scope, so the two are indistinguishable.
        if deleted == 0 {
            return Err(DomainError::membership_not_found(format!(
                "({group_id}, {resource_type}, {resource_id})"
            )));
        }
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-4
        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-5
        Ok(())
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-5
    }

    /// List memberships with `OData` filtering and pagination (AuthZ-scoped).
    pub async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError> {
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-1
        // Actor sends GET /api/resource-group/v1/memberships?$filter={expr}&cursor={token}&limit={n}
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-1
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-2
        // Parse OData $filter (handled by ODataQuery parameter)
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-2
        // AuthZ gate: verify the caller can list memberships
        let scope = self
            .enforcer
            .access_scope_with(
                ctx,
                &RG_MEMBERSHIP_RESOURCE,
                "list",
                None,
                &AccessRequest::new().allow_empty_scope(true),
            )
            .await
            .map_err(DomainError::from)?;

        let conn = self.conn()?;
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-3
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-4
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-5
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-6
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-7
        #[allow(clippy::let_and_return)]
        let result = self
            .membership_repo
            .list_memberships(&conn, &scope, query)
            .await;
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-7
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-6
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-5
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-4
        // @cpt-end:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-3
        result
    }

    /// List memberships without `AuthZ` enforcement (private API, no tenant scoping).
    ///
    /// **Internal API** — never expose this through a REST handler. Backs the
    /// membership read (`ResourceGroupReadHierarchy::list_memberships`): an
    /// in-process `AuthZ` PDP resolves a subject's group memberships while
    /// *being* the PDP, so it cannot re-enter the `PolicyEnforcer` (would
    /// recurse). Mirrors `add_membership_unscoped` — only the enforcer gate is
    /// skipped; the caller supplies any subject/tenant `OData` filter.
    pub async fn list_memberships_unscoped(
        &self,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError> {
        let conn = self.conn()?;
        self.membership_repo
            .list_memberships(&conn, &AccessScope::allow_all(), query)
            .await
    }
}

// -- MembershipAdder trait implementation for seeding --

#[async_trait::async_trait]
impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    crate::domain::seeding::MembershipAdder for MembershipService<GR, TR, MR>
{
    async fn add_membership(
        &self,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), DomainError> {
        // Seeding runs at gear init, before any caller `SecurityContext`
        // exists; using `SecurityContext::anonymous()` here would gate the
        // path on whether anonymous subjects are allowed to create
        // memberships, which is brittle and outright fails in locked-down
        // deployments. Use the dedicated unscoped entry point — domain
        // invariants still run, only the `PolicyEnforcer` gate is skipped.
        self.add_membership_unscoped(group_id, resource_type, resource_id)
            .await
            .map(|_| ())
    }
}
// @cpt-end:cpt-cf-resource-group-dod-membership-service:p1:inst-full

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_security::{ScopeConstraint, ScopeFilter};

    #[test]
    fn group_scope_for_membership_fails_closed() {
        let tenant = Uuid::now_v7();
        let group = Uuid::now_v7();

        let unconstrained = group_scope_for_membership(&AccessScope::allow_all());
        assert!(unconstrained.is_unconstrained());

        let tenant_only = AccessScope::for_tenant(tenant);
        assert_eq!(group_scope_for_membership(&tenant_only), tenant_only);

        let by_resource = group_scope_for_membership(&AccessScope::for_resource(group));
        assert_eq!(by_resource, AccessScope::deny_all());

        let mixed = AccessScope::from_constraints(vec![
            ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                pep_properties::OWNER_TENANT_ID,
                vec![tenant],
            )]),
            ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                pep_properties::RESOURCE_ID,
                vec![group],
            )]),
        ]);
        assert_eq!(group_scope_for_membership(&mixed), AccessScope::deny_all());
    }

    /// The narrowed scope is built from live branches only: removed empty
    /// branches of the source are not carried over.
    #[test]
    fn group_scope_for_membership_does_not_carry_removed_branches() {
        let tenant = Uuid::now_v7();
        let dead = ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
            ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![]),
        ]);
        let all_empty = AccessScope::from_constraints(vec![dead.clone()]);
        assert_eq!(
            group_scope_for_membership(&all_empty),
            AccessScope::deny_all()
        );

        let mixed = AccessScope::from_constraints(vec![
            dead,
            ScopeConstraint::new(vec![ScopeFilter::eq(
                pep_properties::OWNER_TENANT_ID,
                tenant,
            )]),
        ]);
        let narrowed = group_scope_for_membership(&mixed);
        assert_eq!(narrowed.constraints(), mixed.constraints());
        assert!(narrowed.original_constraints().eq(narrowed.constraints()));
    }
}
