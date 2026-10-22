//! The assignment surface of the [`ManagementService`], which reads the
//! tenant hierarchy for it. Every method backs one method of
//! [`PolicyManagementClientV1`](policy_engine_sdk::PolicyManagementClientV1)
//! with the same parameters and result.
//!
//! # Barrier rule
//!
//! The assignment surfaces **respect barriers**, by declaration. From the
//! caller's own tenant, a target tenant is
//!
//! - *reachable* when the hierarchy reaches it with [`BarrierMode::Respect`];
//! - *behind a barrier* when it is reachable only with
//!   [`BarrierMode::Ignore`];
//! - *outside* otherwise (a sibling subtree, an ancestor, an unknown tenant).
//!
//! Creating an assignment needs a reachable target - anything else is
//! refused with `TENANT_BOUNDARY`. Reading and changing an existing
//! assignment at a tenant behind a barrier is refused with `TENANT_BOUNDARY`;
//! at a tenant outside the caller's reach entirely the assignment is reported
//! not found, since a forbidden target and an absent one must look the same.
//!
//! # Mutations
//!
//! Assign, update and unassign are authorised for the publish capability
//! against the caller's context (the assigning administrator's tenant as the
//! `owner_tenant_id` property) and write the change in one transaction,
//! recording the acting subject (`created_by`, `updated_by`) and logging it.
//! Assign additionally requires the bundle to be readable by the caller, so
//! an assignment never names content its assigner cannot see.
//!
//! Unassigning an assignment that does not exist (or is not visible to the
//! caller) succeeds without changes; one the caller sees but may not remove
//! is refused, never reported withdrawn while it still exists.
//!
//! # Dormant assignments
//!
//! An assignment applies only while its owner tenant reaches its target with
//! barriers respected (the decision path checks the target's ancestor chain).
//! Once a barrier is raised between them - the target, or a tenant between
//! it and the owner, becomes self-managed - the assignment is dormant: its
//! owner can no longer change or withdraw it (`TENANT_BOUNDARY`), and an
//! administrator of the target tenant (publish on the target tenant, which
//! the caller reaches) may withdraw it.

use policy_engine_sdk::gts::permissions::Capability;
use policy_engine_sdk::management::{
    self as sdk, AssignmentSpec, ManagementError, ManagementResource,
};
use tenant_resolver_sdk::{BarrierMode, TenantId};
use toolkit_macros::domain_model;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::ContentStore;
use super::authz::{AccessTarget, Authorization, AuthzError};
use super::error::{self, Subject};
use super::service::{ManagementService, now};
use crate::domain::model::{Assignment, AssignmentId, Bundle};
use crate::domain::ports::PortError;
use crate::domain::repos::{AssignmentRepository, RepoError, conflict};

/// Where a tenant lies from the caller's own tenant (module documentation,
/// "Barrier rule").
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Reachable with barriers respected.
    Reachable,
    /// Reachable only when barriers are ignored.
    BehindBarrier,
    /// Not reachable at all (or unknown to the caller's context).
    Outside,
}

impl<S: ContentStore> ManagementService<S> {
    // -- Assignments ---------------------------------------------------------

    /// Assigns a bundle to a tenant. Capability: publish (and read on the
    /// bundle).
    ///
    /// # Errors
    ///
    /// `TENANT_BOUNDARY` for a target not reachable from the caller's
    /// context with barriers respected; `ASSIGNMENT_EXISTS`; not found for
    /// the bundle; `CAPABILITY_DENIED`.
    pub async fn assign(
        &self,
        ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<sdk::Assignment, ManagementError> {
        let caller_tenant = ctx.subject_tenant_id();
        let bundle_id = spec.bundle_id;
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Publish,
                AccessTarget::owned_by(caller_tenant).resource(bundle_id),
                Subject::Bundle(bundle_id),
            )
            .await?;
        self.readable_bundle(ctx, &bundle).await?;
        if self.reach(ctx, spec.tenant_id).await? != Reach::Reachable {
            return Err(tenant_boundary());
        }
        let at = now();
        let assignment = Assignment {
            id: AssignmentId(Uuid::new_v4()),
            bundle_id,
            tenant_id: spec.tenant_id,
            owner_tenant_id: caller_tenant,
            enforce: spec.enforce,
            created_at: at,
            created_by: ctx.subject_id(),
            updated_at: at,
            updated_by: ctx.subject_id(),
        };
        let created = self
            .commit(ctx, &auth, move |tx, env| {
                Box::pin(async move {
                    let inserted = env
                        .store
                        .assignments()
                        .insert(tx, &env.scope, &assignment)
                        .await
                        .map_err(|e| match e {
                            RepoError::NotFound => error::not_found(Subject::Bundle(bundle_id)),
                            RepoError::Conflict {
                                reason: conflict::ASSIGNMENT_EXISTS,
                            } => ManagementError::AssignmentExists {
                                bundle_id,
                                tenant_id: assignment.tenant_id,
                            },
                            other => assignment_repo(other, assignment.id),
                        })?;
                    Ok(assignment_to_sdk(&inserted))
                })
            })
            .await?;
        log_change("assigned", ctx, &created);
        Ok(created)
    }

    /// Reads an assignment. Capability: read on its bundle.
    ///
    /// # Errors
    ///
    /// Not found (absent, its bundle not readable, or its tenant outside
    /// the caller's reach); `TENANT_BOUNDARY` for an assignment at a tenant
    /// behind a barrier.
    pub async fn get_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<sdk::Assignment, ManagementError> {
        let current = self.prefetch_assignment(assignment_id).await?;
        let bundle = self
            .prefetch_bundle(current.bundle_id)
            .await
            .map_err(|e| absent_as_assignment(e, assignment_id))?;
        self.readable_bundle(ctx, &bundle)
            .await
            .map_err(|e| absent_as_assignment(e, assignment_id))?;
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        Ok(assignment_to_sdk(&current))
    }

    /// Sets whether an assignment enforces its bundle's denials. Capability:
    /// publish.
    ///
    /// # Errors
    ///
    /// `TENANT_BOUNDARY`; not found; `CAPABILITY_DENIED`.
    pub async fn update_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
        enforce: bool,
    ) -> Result<sdk::Assignment, ManagementError> {
        let current = self.prefetch_assignment(assignment_id).await?;
        let auth = self.assignment_access(ctx, &current).await?;
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        let updated = self
            .commit(ctx, &auth, move |tx, env| {
                Box::pin(async move {
                    let updated = env
                        .store
                        .assignments()
                        .set_enforce(
                            tx,
                            &env.scope,
                            assignment_id,
                            enforce,
                            env.actor_id,
                            env.now,
                        )
                        .await
                        .map_err(|e| assignment_repo(e, assignment_id))?;
                    Ok(assignment_to_sdk(&updated))
                })
            })
            .await?;
        log_change("assignment updated", ctx, &updated);
        Ok(updated)
    }

    /// Withdraws an assignment. One that does not exist (or that the caller
    /// may not see) is already withdrawn: success, nothing changed. A dormant
    /// assignment (module documentation) may also be withdrawn by an
    /// administrator of its target tenant. Capability: publish.
    ///
    /// # Errors
    ///
    /// `TENANT_BOUNDARY`; `CAPABILITY_DENIED`, including for an assignment
    /// the caller's grant does not cover - never a success that leaves it in
    /// place.
    pub async fn unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<(), ManagementError> {
        let Some(current) = self.find_assignment(assignment_id).await? else {
            return Ok(());
        };
        let auth = match self.assignment_access(ctx, &current).await {
            Ok(auth) => auth,
            Err(err) => {
                if let Dormant::Withdrawn = self.withdraw_dormant(ctx, &current).await? {
                    return Ok(());
                }
                return match err {
                    // Not visible to the caller: as good as absent.
                    ManagementError::NotFound { .. } => Ok(()),
                    other => Err(other),
                };
            }
        };
        // The caller administers it: a tenant out of reach is a refusal, never
        // a success that leaves the assignment enforcing.
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        match self.delete_assignment(ctx, &auth, &current).await? {
            Deleted::Removed | Deleted::Gone => Ok(()),
            // Authorised, but the grant does not cover this row.
            Deleted::OutOfScope => match self.withdraw_dormant(ctx, &current).await? {
                Dormant::Withdrawn => Ok(()),
                Dormant::NotApplicable => Err(ManagementError::CapabilityDenied),
            },
        }
    }

    // -- Shared steps --------------------------------------------------------

    /// Read access to `bundle`, enforcing the scope the authorisation
    /// returned: a grant can carry row constraints, so the prefetched bundle
    /// must be within them, not merely exist.
    async fn readable_bundle(
        &self,
        ctx: &SecurityContext,
        bundle: &Bundle,
    ) -> Result<(), ManagementError> {
        let scope = self
            .read_access(ctx, bundle, bundle.id, Subject::Bundle(bundle.id))
            .await?;
        let conn = self.conn()?;
        self.visible_bundle(&conn, &scope, bundle.id).await?;
        Ok(())
    }

    /// Withdraws `current` on behalf of an administrator of its target
    /// tenant when it is dormant (its owner no longer reaches the target with
    /// barriers respected) and the caller reaches the target and holds the
    /// publish capability there - a grant whose scope covers this row as
    /// content of the target tenant. [`Dormant::NotApplicable`] when any of
    /// that does not hold; the caller then answers as it would without this
    /// path.
    ///
    /// # Errors
    ///
    /// `service_unavailable` when the hierarchy or the PDP cannot answer;
    /// `CAPABILITY_DENIED` when even the row-confined delete misses the row.
    async fn withdraw_dormant(
        &self,
        ctx: &SecurityContext,
        current: &Assignment,
    ) -> Result<Dormant, ManagementError> {
        if !self.is_dormant(ctx, current).await?
            || self.reach(ctx, current.tenant_id).await? != Reach::Reachable
        {
            return Ok(Dormant::NotApplicable);
        }
        // A decision for this one prefetched row. A grant can come with
        // constraints (another tenant's content, other rows): it counts only
        // when they admit this row as content of the target tenant.
        let target = AccessTarget::owned_by(current.tenant_id)
            .resource(current.id)
            .prefetched();
        match self.authz.authorize(ctx, Capability::Publish, target).await {
            Ok(auth) if auth.covers(current.tenant_id, current.id.0) => {}
            Ok(_) | Err(AuthzError::Denied) => return Ok(Dormant::NotApplicable),
            Err(err) => return Err(error::authz(err, || ManagementError::CapabilityDenied)),
        }
        // The grant is on the target tenant, not the owner's: the write is
        // confined to this one row of its owner.
        let auth = Authorization {
            scope: AccessScope::single(ScopeConstraint::new(vec![
                ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, current.owner_tenant_id),
                ScopeFilter::eq(pep_properties::RESOURCE_ID, current.id.0),
            ])),
        };
        match self.delete_assignment(ctx, &auth, current).await? {
            Deleted::Removed | Deleted::Gone => Ok(Dormant::Withdrawn),
            Deleted::OutOfScope => Err(ManagementError::CapabilityDenied),
        }
    }

    /// Whether `current`'s owner no longer reaches its target with barriers
    /// respected: the owner is not in the target's ancestor chain.
    async fn is_dormant(
        &self,
        ctx: &SecurityContext,
        current: &Assignment,
    ) -> Result<bool, ManagementError> {
        match self
            .hierarchy
            .ancestors(ctx, TenantId(current.tenant_id))
            .await
        {
            Ok(chain) => Ok(!chain.contains(&current.owner_tenant_id)),
            Err(PortError::NotFound(_)) => Ok(false),
            Err(err) => Err(error::unavailable(err)),
        }
    }

    /// Deletes `current` under `auth`, telling a row removed concurrently
    /// apart from one the scope does not cover.
    async fn delete_assignment(
        &self,
        ctx: &SecurityContext,
        auth: &Authorization,
        current: &Assignment,
    ) -> Result<Deleted, ManagementError> {
        let assignment_id = current.id;
        let deleted = self
            .commit(ctx, auth, move |tx, env| {
                Box::pin(async move {
                    env.store
                        .assignments()
                        .delete(tx, &env.scope, assignment_id)
                        .await
                        .map_err(|e| assignment_repo(e, assignment_id))?;
                    Ok(())
                })
            })
            .await;
        match deleted {
            Ok(()) => {
                log_change("unassigned", ctx, &assignment_to_sdk(current));
                Ok(Deleted::Removed)
            }
            Err(ManagementError::NotFound { .. }) => {
                if self.find_assignment(assignment_id).await?.is_some() {
                    Ok(Deleted::OutOfScope)
                } else {
                    Ok(Deleted::Gone)
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Where `tenant` lies from the caller's own tenant.
    ///
    /// # Errors
    ///
    /// `service_unavailable` when the hierarchy cannot answer.
    pub(super) async fn reach(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
    ) -> Result<Reach, ManagementError> {
        let from = ctx.subject_tenant_id();
        if from.is_nil() {
            return Ok(Reach::Outside);
        }
        if self
            .reachable(ctx, from, tenant, BarrierMode::Respect)
            .await?
        {
            return Ok(Reach::Reachable);
        }
        if self
            .reachable(ctx, from, tenant, BarrierMode::Ignore)
            .await?
        {
            return Ok(Reach::BehindBarrier);
        }
        Ok(Reach::Outside)
    }

    /// Whether `target` is reachable from `from` under `barrier`, whatever
    /// its status; a tenant the provider does not know is not reachable.
    pub(super) async fn reachable(
        &self,
        ctx: &SecurityContext,
        from: Uuid,
        target: Uuid,
        barrier: BarrierMode,
    ) -> Result<bool, ManagementError> {
        match self
            .hierarchy
            .is_reachable(ctx, TenantId(from), TenantId(target), barrier)
            .await
        {
            Ok(reachable) => Ok(reachable),
            Err(PortError::NotFound(_)) => Ok(false),
            Err(err) => Err(error::unavailable(err)),
        }
    }

    /// The assignment under the gear's own scope, only to learn its owner
    /// and bundle for authorisation (the prefetch pattern).
    async fn find_assignment(
        &self,
        id: AssignmentId,
    ) -> Result<Option<Assignment>, ManagementError> {
        let conn = self.conn()?;
        self.store
            .assignments()
            .get(&conn, &AccessScope::allow_all(), id)
            .await
            .map_err(|e| assignment_repo(e, id))
    }

    /// [`Self::find_assignment`], absent reported as not found.
    async fn prefetch_assignment(&self, id: AssignmentId) -> Result<Assignment, ManagementError> {
        self.find_assignment(id)
            .await?
            .ok_or_else(|| assignment_not_found(id))
    }

    /// The assignment capability on an existing assignment; an absence
    /// names the assignment.
    async fn assignment_access(
        &self,
        ctx: &SecurityContext,
        assignment: &Assignment,
    ) -> Result<Authorization, ManagementError> {
        let owner = assignment.owner_tenant_id;
        self.mutation_access(
            ctx,
            Capability::Publish,
            AccessTarget::owned_by(owner).resource(assignment.id),
            Subject::Tenant(owner),
        )
        .await
        .map_err(|e| absent_as_assignment(e, assignment.id))
    }

    /// The barrier rule for a change to `assignment_id`, an existing
    /// assignment at `tenant`: behind a barrier is `TENANT_BOUNDARY`, outside
    /// the caller's reach entirely is not found (the mutation surfaces mirror
    /// the read).
    async fn guard_tenant(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
        tenant: Uuid,
    ) -> Result<(), ManagementError> {
        match self.reach(ctx, tenant).await? {
            Reach::Reachable => Ok(()),
            Reach::BehindBarrier => Err(tenant_boundary()),
            Reach::Outside => Err(assignment_not_found(assignment_id)),
        }
    }
}

/// What [`ManagementService::withdraw_dormant`] did.
enum Dormant {
    /// The assignment was dormant and the caller withdrew it.
    Withdrawn,
    /// The path does not apply: not dormant, target out of the caller's
    /// reach, or no grant covering the target tenant.
    NotApplicable,
}

/// What a scoped delete of an assignment did.
enum Deleted {
    /// The row was removed.
    Removed,
    /// The row was already gone (removed concurrently).
    Gone,
    /// The row exists but the scope does not cover it.
    OutOfScope,
}

/// The record of an assignment change: who, what, where.
fn log_change(change: &'static str, ctx: &SecurityContext, assignment: &sdk::Assignment) {
    tracing::info!(
        actor_id = %ctx.subject_id(),
        actor_tenant_id = %ctx.subject_tenant_id(),
        assignment_id = %assignment.id.0,
        bundle_id = %assignment.bundle_id.0,
        tenant_id = %assignment.tenant_id,
        owner_tenant_id = %assignment.owner_tenant_id,
        enforce = assignment.enforce,
        "policy-engine: {change}"
    );
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The target tenant lies behind a self-managed barrier from the caller's
/// context (or, for a new assignment, is not reachable from it).
#[must_use]
pub fn tenant_boundary() -> ManagementError {
    ManagementError::TenantBoundary
}

/// An assignment that does not exist or the caller may not see.
#[must_use]
pub fn assignment_not_found(id: AssignmentId) -> ManagementError {
    ManagementError::NotFound {
        resource: ManagementResource::Assignment,
        id: id.0,
    }
}

/// Re-labels an absence reported for the assignment's bundle or owner as
/// the assignment's own, so a refusal never names content behind it.
fn absent_as_assignment(err: ManagementError, id: AssignmentId) -> ManagementError {
    if matches!(err, ManagementError::NotFound { .. }) {
        assignment_not_found(id)
    } else {
        err
    }
}

fn assignment_repo(err: RepoError, id: AssignmentId) -> ManagementError {
    match err {
        RepoError::NotFound => assignment_not_found(id),
        other => error::repo(other, Subject::Tenant(id.0)),
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// The SDK view of an assignment.
#[must_use]
pub fn assignment_to_sdk(assignment: &Assignment) -> sdk::Assignment {
    sdk::Assignment {
        id: assignment.id,
        bundle_id: assignment.bundle_id,
        tenant_id: assignment.tenant_id,
        owner_tenant_id: assignment.owner_tenant_id,
        enforce: assignment.enforce,
        created_at: assignment.created_at,
        created_by: assignment.created_by,
        updated_at: assignment.updated_at,
        updated_by: assignment.updated_by,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "assignments_tests.rs"]
mod assignments_tests;
