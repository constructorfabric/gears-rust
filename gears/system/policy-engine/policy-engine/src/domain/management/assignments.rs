//! [`GovernanceService`]: the assignment surface of the management service.
//!
//! The service composes the bundle-and-version [`ManagementService`] (its
//! authorizer, store, validator and transaction helpers) with the
//! tenant hierarchy. Every method backs one method of
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
//! `owner_tenant_id` property) and write the change in one transaction.
//! Assign additionally requires the bundle to be readable by the caller, so an assignment never names content its assigner cannot see.
//! Unassigning an assignment that does not exist (or is not visible to the
//! caller) succeeds without changes.

use std::sync::Arc;

use policy_engine_sdk::gts::permissions::Capability;
use policy_engine_sdk::management::{self as sdk, AssignmentSpec, ManagementError, reason};
use tenant_resolver_sdk::BarrierMode;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::ContentStore;
use super::authz::{AccessTarget, Authorization};
use super::error::{self, AssignmentResourceError, Subject};
use super::service::{ManagementService, now};
use crate::domain::model::{Assignment, AssignmentId, BundleId};
use crate::domain::ports::{HierarchyPort, PortError};
use crate::domain::repos::{AssignmentRepository, RepoError, conflict};

/// Assignments over a [`ManagementService`].
#[domain_model]
pub struct GovernanceService<S: ContentStore> {
    pub(super) core: Arc<ManagementService<S>>,
    pub(super) hierarchy: Arc<dyn HierarchyPort>,
}

impl<S: ContentStore> std::fmt::Debug for GovernanceService<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceService")
            .field("core", &self.core)
            .finish_non_exhaustive()
    }
}

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

impl<S: ContentStore> GovernanceService<S> {
    /// Composes `core` with `hierarchy`: reachability and ancestry, read
    /// under the caller's context (the uncached adapter: management reads
    /// must see the provider's answer).
    #[must_use]
    pub fn new(core: Arc<ManagementService<S>>, hierarchy: Arc<dyn HierarchyPort>) -> Self {
        Self { core, hierarchy }
    }

    /// The bundle-and-version service.
    #[must_use]
    pub fn core(&self) -> &Arc<ManagementService<S>> {
        &self.core
    }

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
        let core = &self.core;
        let bundle_id = spec.bundle_id;
        let bundle = core.prefetch_bundle(bundle_id).await?;
        let auth = core
            .mutation_access(
                ctx,
                Capability::Publish,
                AccessTarget::owned_by(caller_tenant).resource(bundle_id),
                caller_tenant,
                Subject::Bundle(bundle_id),
            )
            .await?;
        core.read_access(ctx, &bundle, bundle_id, Subject::Bundle(bundle_id))
            .await?;
        if self.reach(ctx, spec.tenant_id).await? != Reach::Reachable {
            return Err(tenant_boundary());
        }
        let at = now();
        let assignment = Assignment {
            id: AssignmentId(Uuid::new_v4()),
            bundle_id: BundleId(bundle_id),
            tenant_id: spec.tenant_id,
            owner_tenant_id: caller_tenant,
            enforce: spec.enforce,
            created_at: at,
            updated_at: at,
        };
        core.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let inserted = env
                    .store
                    .assignments()
                    .insert(tx, &env.scope, &assignment)
                    .await
                    .map_err(|e| match e {
                        RepoError::NotFound => error::not_found(Subject::Bundle(bundle_id)),
                        other => assignment_repo(other, assignment.id.0),
                    })?;
                Ok(assignment_to_sdk(&inserted))
            })
        })
        .await
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
        assignment_id: Uuid,
    ) -> Result<sdk::Assignment, ManagementError> {
        let current = self.prefetch_assignment(assignment_id).await?;
        let bundle = self
            .core
            .prefetch_bundle(current.bundle_id.0)
            .await
            .map_err(|e| absent_as_assignment(e, assignment_id))?;
        self.core
            .read_access(ctx, &bundle, assignment_id, Subject::Bundle(bundle.id.0))
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
        assignment_id: Uuid,
        enforce: bool,
    ) -> Result<sdk::Assignment, ManagementError> {
        let current = self.prefetch_assignment(assignment_id).await?;
        let auth = self.assignment_access(ctx, &current).await?;
        let core = &self.core;
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        core.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let updated = env
                    .store
                    .assignments()
                    .set_enforce(
                        tx,
                        &env.scope,
                        AssignmentId(assignment_id),
                        enforce,
                        env.now,
                    )
                    .await
                    .map_err(|e| assignment_repo(e, assignment_id))?;
                Ok(assignment_to_sdk(&updated))
            })
        })
        .await
    }

    /// Withdraws an assignment. One that does not exist (or that the caller
    /// may not see) is already withdrawn: success, nothing changed.
    /// Capability: publish.
    ///
    /// # Errors
    ///
    /// `TENANT_BOUNDARY`; `CAPABILITY_DENIED`.
    pub async fn unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: Uuid,
    ) -> Result<(), ManagementError> {
        let core = &self.core;
        let Some(current) = self.find_assignment(assignment_id).await? else {
            return Ok(());
        };
        let withdrawn = async {
            let auth = self.assignment_access(ctx, &current).await?;
            self.guard_tenant(ctx, assignment_id, current.tenant_id)
                .await?;
            core.commit(ctx, &auth, move |tx, env| {
                Box::pin(async move {
                    env.store
                        .assignments()
                        .delete(tx, &env.scope, AssignmentId(assignment_id))
                        .await
                        .map_err(|e| assignment_repo(e, assignment_id))?;
                    Ok(())
                })
            })
            .await
        }
        .await;
        match withdrawn {
            Err(err) if err.status_code() != 404 => Err(err),
            _ => Ok(()),
        }
    }

    // -- Shared steps --------------------------------------------------------

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
            .is_reachable(ctx, from, target, barrier)
            .await
        {
            Ok(reachable) => Ok(reachable),
            Err(PortError::NotFound(_)) => Ok(false),
            Err(err) => Err(error::unavailable(err)),
        }
    }

    /// The assignment under the gear's own scope, only to learn its owner
    /// and bundle for authorisation (the prefetch pattern).
    async fn find_assignment(&self, id: Uuid) -> Result<Option<Assignment>, ManagementError> {
        let conn = self.core.conn()?;
        self.core
            .store
            .assignments()
            .get(&conn, &AccessScope::allow_all(), AssignmentId(id))
            .await
            .map_err(|e| assignment_repo(e, id))
    }

    /// [`Self::find_assignment`], absent reported as not found.
    async fn prefetch_assignment(&self, id: Uuid) -> Result<Assignment, ManagementError> {
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
        self.core
            .mutation_access(
                ctx,
                Capability::Publish,
                AccessTarget::owned_by(owner).resource(assignment.id.0),
                owner,
                Subject::Tenant(owner),
            )
            .await
            .map_err(|e| absent_as_assignment(e, assignment.id.0))
    }

    /// The barrier rule for a change to `assignment_id`, an existing
    /// assignment at `tenant`: behind a barrier is `TENANT_BOUNDARY`, outside
    /// the caller's reach entirely is not found (the mutation surfaces mirror
    /// the read).
    async fn guard_tenant(
        &self,
        ctx: &SecurityContext,
        assignment_id: Uuid,
        tenant: Uuid,
    ) -> Result<(), ManagementError> {
        match self.reach(ctx, tenant).await? {
            Reach::Reachable => Ok(()),
            Reach::BehindBarrier => Err(tenant_boundary()),
            Reach::Outside => Err(assignment_not_found(assignment_id)),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The target tenant lies behind a self-managed barrier from the caller's
/// context (or, for a new assignment, is not reachable from it).
#[must_use]
pub fn tenant_boundary() -> ManagementError {
    AssignmentResourceError::permission_denied()
        .with_reason(reason::TENANT_BOUNDARY)
        .create()
}

/// A second assignment of the bundle to the same tenant.
#[must_use]
pub fn assignment_exists() -> ManagementError {
    AssignmentResourceError::already_exists(format!(
        "{}: the bundle is already assigned to this tenant; update that assignment instead",
        reason::ASSIGNMENT_EXISTS
    ))
    .with_resource("assignment".to_owned())
    .create()
}

/// An assignment that does not exist or the caller may not see.
#[must_use]
pub fn assignment_not_found(id: Uuid) -> ManagementError {
    AssignmentResourceError::not_found("assignment not found")
        .with_resource(id.to_string())
        .create()
}

/// Re-labels an absence reported for the assignment's bundle or owner as
/// the assignment's own, so a refusal never names content behind it.
fn absent_as_assignment(err: ManagementError, id: Uuid) -> ManagementError {
    if err.status_code() == 404 {
        assignment_not_found(id)
    } else {
        err
    }
}

fn assignment_repo(err: RepoError, id: Uuid) -> ManagementError {
    match err {
        RepoError::NotFound => assignment_not_found(id),
        RepoError::Conflict {
            reason: conflict::ASSIGNMENT_EXISTS,
        } => assignment_exists(),
        other => error::repo(other, Subject::Tenant(id)),
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// The SDK view of an assignment.
#[must_use]
pub fn assignment_to_sdk(assignment: &Assignment) -> sdk::Assignment {
    sdk::Assignment {
        id: assignment.id.0,
        bundle_id: assignment.bundle_id.0,
        tenant_id: assignment.tenant_id,
        owner_tenant_id: assignment.owner_tenant_id,
        enforce: assignment.enforce,
        created_at: assignment.created_at,
        updated_at: assignment.updated_at,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "assignments_tests.rs"]
mod assignments_tests;
