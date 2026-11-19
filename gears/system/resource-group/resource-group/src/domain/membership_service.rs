// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-membership-service:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-membership:p1
//! Domain service for resource group membership management.
//!
//! Implements business rules for adding, removing, and listing memberships
//! between resources and groups. Delegates persistence to the infra layer.

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
use resource_group_sdk::{
    GROUP_MEMBERSHIP_RESOURCE_TYPE, MAX_MEMBERSHIP_RESOURCE_ID_BYTES,
    models::ResourceGroupMembership,
};
use toolkit_db::secure::TxConfig;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use tracing::debug;

use crate::domain::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait};
use crate::domain::type_catalog::RgTypeCatalog;

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
pub struct MembershipService<GR: GroupRepositoryTrait, MR: MembershipRepositoryTrait> {
    db: Arc<DbProvider>,
    enforcer: PolicyEnforcer,
    group_repo: Arc<GR>,
    types: Arc<dyn RgTypeCatalog>,
    membership_repo: Arc<MR>,
}

impl<GR: GroupRepositoryTrait, MR: MembershipRepositoryTrait> MembershipService<GR, MR> {
    /// Create a new `MembershipService` with the given database provider
    /// and `PolicyEnforcer` for AuthZ-scoped queries.
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        enforcer: PolicyEnforcer,
        group_repo: Arc<GR>,
        types: Arc<dyn RgTypeCatalog>,
        membership_repo: Arc<MR>,
    ) -> Self {
        Self {
            db,
            enforcer,
            group_repo,
            types,
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

        self.add_membership_inner(ctx, &scope, group_id, resource_type, resource_id)
            .await
    }

    /// Post-authz body of `add_membership`.
    async fn add_membership_inner(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, DomainError> {
        // Check the resource identifier's UTF-8 size before any registry or
        // write work. It is part of the membership primary key (with the group
        // UUID and the type's dictionary key); the portable bound leaves
        // PostgreSQL B-tree tuple room. The type identifier is stored as a
        // key, so its length does not count.
        if resource_id.len() > MAX_MEMBERSHIP_RESOURCE_ID_BYTES {
            return Err(DomainError::validation(format!(
                "Membership resource_id must be at most \
                 {MAX_MEMBERSHIP_RESOURCE_ID_BYTES} UTF-8 bytes"
            )));
        }
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
        //
        // The type rule comes from the types registry, read before the
        // transaction opens (no network I/O under an open transaction): the
        // group's type is immutable, so it is read first -- under the caller's
        // scope, so a group outside it is "not found" before any type-shaped
        // answer -- and the transaction re-reads the group and confirms it
        // still has that type. Narrowing a type's `allowed_membership_types`
        // no longer races this insert: that guarantee went with RG's own
        // catalogue (T38b).
        let db = self.db.db();
        let group_scope = group_scope_for_membership(scope);
        let group_type = {
            let conn = self.conn()?;
            self.group_repo
                .find_model_by_id_scoped(&conn, &group_scope, group_id)
                .await?
                .ok_or(DomainError::GroupNotFound { id: group_id })?
                .code
        };
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7
        // The member resource type must be registered and Active (any GTS
        // type); the group's type supplies `allowed_membership_types`.
        let rule = self
            .types
            .get_membership_rule(ctx, &group_type, resource_type)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8
        if !rule.allows_member(resource_type) {
            return Err(DomainError::validation(format!(
                "Resource type '{resource_type}' is not in allowed_membership_types for group type '{}'",
                rule.code
            )));
        }
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8

        // The definition the rule came from; the transaction's row must be
        // of exactly this type.
        let group_type = rule.code;
        let group_repo = self.group_repo.clone();
        let membership_repo = self.membership_repo.clone();
        let resource_type_owned = resource_type.to_owned();
        let resource_id_owned = resource_id.to_owned();

        db.transaction_with_retry(TxConfig::serializable(), DomainError::db_err, |tx| {
            let group_repo = group_repo.clone();
            let membership_repo = membership_repo.clone();
            let group_scope = group_scope.clone();
            let group_type = group_type.clone();
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
                // The rule above was read for this type; a group deleted
                // and re-created under the same id with another type does
                // not inherit it.
                if group_model.code != group_type {
                    return Err(DomainError::conflict(format!(
                        "Group {group_id} changed while the membership was being added"
                    )));
                }
                // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-4
                // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-3

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
                        &resource_type,
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
                // The member type is keyed in RG's identity dictionary
                // only now, after every check that can reject the request:
                // PostgreSQL does not return identity values on rollback, so
                // a key taken earlier would burn a `SMALLINT` per rejected
                // request. The key commits or rolls back with the membership.
                // Reached only after the registry admitted the member type
                // (before `BEGIN`): an existing dictionary row is not an
                // admission.
                membership_repo
                    .insert(tx, group_id, &resource_type, &resource_id)
                    .await
                // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-12
                // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-11
            })
        })
        .await?;

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-13
        // The values just written, in GTS identifier terms for the SDK model.
        Ok(ResourceGroupMembership {
            group_id,
            resource_type: resource_type.to_owned(),
            resource_id: resource_id.to_owned(),
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
        // The type is matched through RG's identity dictionary, without
        // creating a key; removal never consults the types registry, so a
        // deleted or narrowed type never blocks cleanup.
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
            .delete(&conn, &scope, group_id, resource_type, resource_id)
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
    /// recurse). Only the enforcer gate is skipped; the caller supplies any
    /// subject/tenant `OData` filter.
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
