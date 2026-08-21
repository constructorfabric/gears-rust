// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-membership-service:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-testing-membership:p1
//! Domain service for resource group membership management.
//!
//! Implements business rules for adding, removing, and listing memberships
//! between resources and groups. Delegates persistence to the infra layer.

use std::sync::Arc;

use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use resource_group_sdk::{GROUP_MEMBERSHIP_RESOURCE_TYPE, models::ResourceGroupMembership};
use toolkit_db::secure::TxConfig;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{SecurityContext, pep_properties};
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
        let _scope = self
            .enforcer
            .access_scope(ctx, &RG_MEMBERSHIP_RESOURCE, "create", None)
            .await
            .map_err(DomainError::from)?;

        self.add_membership_inner(group_id, resource_type, resource_id)
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
        self.add_membership_inner(group_id, resource_type, resource_id)
            .await
    }

    /// Shared post-authz body of `add_membership` / `add_membership_unscoped`.
    async fn add_membership_inner(
        &self,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, DomainError> {
        let conn = self.conn()?;

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-3
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-4
        // Verify the group exists and get its type info
        let group_model = self
            .group_repo
            .find_model_by_id(&conn, group_id)
            .await?
            .ok_or(DomainError::GroupNotFound { id: group_id })?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-4
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-3

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5
        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
        // Resolve the GTS type path to a surrogate SMALLINT ID
        let gts_type_id = self
            .type_repo
            .resolve_id(&conn, resource_type)
            .await?
            .ok_or_else(|| {
                DomainError::validation(format!("Unknown resource type: {resource_type}"))
            })?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-6
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-5

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7
        // Load group type's allowed_membership_types and validate
        let allowed = self
            .type_repo
            .load_full_type_by_id(&conn, group_model.gts_type_id)
            .await?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-7

        // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8
        if !allowed
            .allowed_membership_types
            .iter()
            .any(|m| m == resource_type)
        {
            return Err(DomainError::validation(format!(
                "Resource type '{resource_type}' is not in allowed_membership_types for group type '{}'",
                allowed.code
            )));
        }
        // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-8

        // Tenant compatibility via the guard table `resource_membership_tenant`,
        // run in the same transaction as the membership insert below: a failed
        // insert (e.g. a duplicate membership) rolls the guard claim back
        // instead of leaving a claim behind with no membership to justify it
        // (RG-01, and the guard-lifecycle fix that came with it).
        let db = self.db.db();
        let membership_repo = self.membership_repo.clone();
        let resource_type_owned = resource_type.to_owned();
        let resource_id_owned = resource_id.to_owned();
        let target_tenant_id = group_model.tenant_id;

        let model = db
            .transaction_with_retry(TxConfig::default(), DomainError::db_err, |tx| {
                let membership_repo = membership_repo.clone();
                let resource_type = resource_type_owned.clone();
                let resource_id = resource_id_owned.clone();
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-9
                    // Invoke the tenant compatibility check: reads the
                    // existing guard row, or claims it for `target_tenant_id`
                    // when absent, re-reading the winner on a
                    // unique-violation race.
                    let guard_tenant = membership_repo
                        .ensure_membership_guard(tx, gts_type_id, &resource_id, target_tenant_id)
                        .await?;
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-9

                    // @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-10
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-4
                    // @cpt-begin:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-5
                    if guard_tenant != target_tenant_id {
                        debug!(
                            resource_type = %resource_type,
                            resource_id = %resource_id,
                            "Tenant incompatibility on membership add (guard table)"
                        );
                        // The message stays generic about *which* tenant holds the
                        // claim: `guard_tenant` belongs to a tenant other than the
                        // caller's, and this error reaches the caller verbatim
                        // over the API (api/rest/error.rs), so it must not name it.
                        return Err(DomainError::tenant_incompatibility(format!(
                            "Resource ({resource_type}, {resource_id}) is already \
                             linked to a different tenant"
                        )));
                    }
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-5
                    // @cpt-end:cpt-cf-resource-group-algo-membership-check-tenant-compat:p1:inst-tenant-check-4
                    // @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-10

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
    /// Resolves the GTS type path, verifies the membership exists, and deletes it.
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
        let _scope = self
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
        // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-4
        // Verify the membership exists
        self.membership_repo
            .find_by_composite_key(&conn, group_id, gts_type_id, resource_id)
            .await?
            .ok_or_else(|| {
                DomainError::membership_not_found(format!(
                    "({group_id}, {resource_type}, {resource_id})"
                ))
            })?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-4

        // Delete the membership, then release the tenant guard once nothing
        // references the resource any more -- both in one transaction, so a
        // membership this call just removed cannot be double-counted by a
        // concurrent `add_membership`'s guard check.
        let db = self.db.db();
        let membership_repo = self.membership_repo.clone();
        let resource_id_owned = resource_id.to_owned();

        db.transaction_with_retry(TxConfig::default(), DomainError::db_err, |tx| {
            let membership_repo = membership_repo.clone();
            let resource_id = resource_id_owned.clone();
            Box::pin(async move {
                membership_repo
                    .delete(tx, group_id, gts_type_id, &resource_id)
                    .await?;

                // @cpt-begin:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-6
                if membership_repo
                    .count_memberships(tx, gts_type_id, &resource_id)
                    .await?
                    == 0
                {
                    membership_repo
                        .delete_membership_guard(tx, gts_type_id, &resource_id)
                        .await?;
                }
                // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-6

                Ok(())
            })
        })
        .await?;
        // @cpt-end:cpt-cf-resource-group-flow-membership-remove:p1:inst-remove-memb-3
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
        let _scope = self
            .enforcer
            .access_scope(ctx, &RG_MEMBERSHIP_RESOURCE, "list", None)
            .await
            .map_err(DomainError::from)?;

        let conn = self.conn()?;
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-3
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-4
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-5
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-6
        // @cpt-begin:cpt-cf-resource-group-flow-membership-list:p1:inst-list-memb-7
        #[allow(clippy::let_and_return)]
        let result = self.membership_repo.list_memberships(&conn, query).await;
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
        self.membership_repo.list_memberships(&conn, query).await
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
