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
//! refused with `TENANT_BOUNDARY`. Changing an existing assignment at a
//! tenant behind a barrier is refused with `TENANT_BOUNDARY`; at a tenant
//! outside the caller's reach entirely the assignment is reported not found,
//! since a forbidden target and an absent one must look the same. Reading
//! follows the visibility rule below instead.
//!
//! # Visibility
//!
//! The listing and the targeted read show a caller, from its own tenant
//! `C`, two kinds of assignment:
//!
//! - those of bundles it may read (an assignment is read as content of its
//!   bundle), at a target in `C`'s subtree with barriers ignored, made by a
//!   tenant on `C`'s side of every barrier: one `C` reaches, or one in `C`'s
//!   barrier-respecting ancestor chain. One at a target behind a barrier is
//!   shown dormant, so its owner sees why it stopped applying;
//! - the dormant assignments at its own tenants it may withdraw: the target
//!   is reachable from `C` with barriers respected and within its publish
//!   grant, and the owner is on the far side of a barrier - neither reached
//!   by `C` nor in its chain. Every target `C` reaches shares that chain
//!   above `C`, so this is exactly "the owner does not reach the target". The
//!   bundle need not be readable: no content is shown.
//!
//! The listing decides both from one subtree read (barriers ignored, with
//! each tenant's barrier flag) and one chain read, not per row; the read of
//! one assignment decides them by point checks on its owner and target, and
//! never reads the subtree. Anything else - a target outside `C`'s subtree
//! altogether included - is not shown, and the read reports it not found. A
//! filter naming content the caller cannot see yields no rows rather than
//! an error. A change the caller may not make to an assignment it sees is
//! refused with `CAPABILITY_DENIED`, never reported absent or done.
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
//! Each mutation runs its checks in a `plan_*` step that only reads, then
//! writes what the plan decided; its dry run
//! ([`check_publish`](ManagementService::check_publish)) is the plan alone.
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
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::ContentStore;
use super::authz::{AccessTarget, Authorization, AuthzError};
use super::error::{self, Subject};
use super::service::{ManagementService, now};
use crate::domain::model::{Assignment, AssignmentId, Bundle};
use crate::domain::ports::{PortError, Subtree};
use crate::domain::repos::{
    AssignmentRepository, AssignmentVisibility, BundleRepository, RepoError, conflict,
};

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
    /// a bundle the caller may not read; `CAPABILITY_DENIED`, including for
    /// a readable bundle whose publish grant does not cover it or the new
    /// assignment.
    pub async fn assign(
        &self,
        ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<sdk::Assignment, ManagementError> {
        let (auth, assignment) = self.plan_assign(ctx, &spec).await?;
        let bundle_id = assignment.bundle_id;
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
                    Ok(inserted)
                })
            })
            .await?;
        log_change("assigned", ctx, &created);
        // Made by the caller's tenant at a tenant it reaches: it applies.
        Ok(assignment_to_sdk(&created, false))
    }

    /// Reads an assignment the caller would see in
    /// [`Self::list_assignments`]. Capability: read, or publish for a
    /// dormant assignment at the caller's tenants.
    ///
    /// # Errors
    ///
    /// Not found when absent or not visible (module documentation,
    /// "Visibility").
    pub async fn get_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<sdk::Assignment, ManagementError> {
        let Some(assignment) = self.find_assignment(assignment_id).await? else {
            return Err(assignment_not_found(assignment_id));
        };
        match self.seen(ctx, &assignment).await? {
            Some(dormant) => Ok(assignment_to_sdk(&assignment, dormant)),
            None => Err(assignment_not_found(assignment_id)),
        }
    }

    /// Whether the caller would see `assignment` in
    /// [`Self::list_assignments`] - `Some` with its dormant flag - decided by
    /// point checks for this one row, never by reading the caller's subtree
    /// (module documentation, "Visibility"): the owner is on the caller's
    /// side of every barrier (in its chain, or reached from it), and either
    /// the caller reads the bundle and reaches the target ignoring barriers,
    /// or it may withdraw a dormant assignment at a target it reaches.
    ///
    /// # Errors
    ///
    /// `service_unavailable` when the PDP or the hierarchy cannot answer.
    async fn seen(
        &self,
        ctx: &SecurityContext,
        assignment: &Assignment,
    ) -> Result<Option<bool>, ManagementError> {
        let read = self.collection_grant(ctx, Capability::Read).await?;
        let publish = self.collection_grant(ctx, Capability::Publish).await?;
        let caller = ctx.subject_tenant_id();
        if (read.is_none() && publish.is_none()) || caller.is_nil() {
            return Ok(None);
        }
        let Some(chain) = self.caller_chain(ctx).await? else {
            return Ok(None);
        };
        let (owner, target) = (assignment.owner_tenant_id, assignment.tenant_id);
        let owner_reaching = chain.contains(&owner)
            || self
                .reachable(ctx, caller, owner, BarrierMode::Respect)
                .await?;
        let visible = if owner_reaching {
            match &read {
                Some(read) => {
                    self.reachable(ctx, caller, target, BarrierMode::Ignore)
                        .await?
                        && self.bundle_readable(&read.scope, assignment).await?
                }
                None => false,
            }
        } else {
            publish
                .as_ref()
                .is_some_and(|auth| auth.covers_tenant(target))
                && self
                    .reachable(ctx, caller, target, BarrierMode::Respect)
                    .await?
        };
        if !visible {
            return Ok(None);
        }
        Ok(Some(self.is_dormant(ctx, assignment).await?))
    }

    /// Whether `assignment`'s bundle is within `read`, as the listing's
    /// bundle subquery decides it.
    async fn bundle_readable(
        &self,
        read: &AccessScope,
        assignment: &Assignment,
    ) -> Result<bool, ManagementError> {
        let conn = self.conn()?;
        Ok(self
            .store
            .bundles()
            .get(&conn, read, assignment.bundle_id)
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(assignment.bundle_id)))?
            .is_some())
    }

    /// The refusal for a change to `current` the caller may not make:
    /// `CAPABILITY_DENIED` when the caller sees the assignment (a refusal
    /// never pretends visible content is absent), `absent` otherwise.
    async fn refusal(
        &self,
        ctx: &SecurityContext,
        current: &Assignment,
        absent: ManagementError,
    ) -> Result<ManagementError, ManagementError> {
        Ok(if self.seen(ctx, current).await?.is_some() {
            ManagementError::CapabilityDenied
        } else {
            absent
        })
    }

    /// Lists the assignments visible to the caller, one cursor page at a
    /// time. Capability: read, or publish for the dormant ones.
    ///
    /// # Errors
    ///
    /// `CAPABILITY_DENIED` when neither is granted; invalid argument for a
    /// malformed query; `SUBTREE_TOO_LARGE` when the caller's subtree is too
    /// large to evaluate the listing in one statement.
    pub async fn list_assignments(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<sdk::Assignment>, ManagementError> {
        let view = self
            .assignment_view(ctx)
            .await?
            .ok_or(ManagementError::CapabilityDenied)?;
        let conn = self.conn()?;
        let page = self
            .store
            .assignments()
            .list_page(&conn, &view.visibility, query)
            .await
            .map_err(|e| error::repo(e, Subject::Tenant(ctx.subject_tenant_id())))?;
        Ok(Page::new(
            page.items.iter().map(|a| view.to_sdk(a)).collect(),
            page.page_info,
        ))
    }

    /// Sets whether an assignment enforces its bundle's denials. Capability:
    /// publish.
    ///
    /// # Errors
    ///
    /// `TENANT_BOUNDARY`; not found; `CAPABILITY_DENIED`;
    /// `service_unavailable` when the hierarchy cannot answer - always before
    /// anything is written.
    pub async fn update_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
        enforce: bool,
    ) -> Result<sdk::Assignment, ManagementError> {
        let (auth, dormant) = self.plan_update(ctx, assignment_id).await?;
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
                    Ok(updated)
                })
            })
            .await?;
        log_change("assignment updated", ctx, &updated);
        Ok(assignment_to_sdk(&updated, dormant))
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
        let Withdrawal::Delete { auth, assignment } =
            self.plan_unassign(ctx, assignment_id).await?
        else {
            return Ok(());
        };
        let deleted = self
            .commit(ctx, &auth, move |tx, env| {
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
                log_change("unassigned", ctx, &assignment);
                Ok(())
            }
            // Gone since the plan read it: withdrawn. Still there, but no
            // longer within the scope the plan saw: refused, never reported
            // withdrawn while it remains.
            Err(ManagementError::NotFound { .. }) => {
                if self.find_assignment(assignment_id).await?.is_some() {
                    Err(ManagementError::CapabilityDenied)
                } else {
                    Ok(())
                }
            }
            Err(err) => Err(err),
        }
    }

    // -- Plans ---------------------------------------------------------------

    /// Every check [`Self::assign`] runs, in its order, and the assignment
    /// it would insert; reads only.
    ///
    /// # Errors
    ///
    /// As [`Self::assign`].
    pub(super) async fn plan_assign(
        &self,
        ctx: &SecurityContext,
        spec: &AssignmentSpec,
    ) -> Result<(Authorization, Assignment), ManagementError> {
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
        // The insert's own checks, in its order: the bundle within the
        // publish grant, the new row within it, and no assignment of the
        // bundle to the tenant yet (by anyone: the pair is unique). The
        // caller has read the bundle by now, so a grant that does not cover
        // the write is a refusal, not an absence.
        let conn = self.conn()?;
        self.visible_bundle(&conn, &auth.scope, bundle_id)
            .await
            .map_err(|e| match e {
                ManagementError::NotFound { .. } => ManagementError::CapabilityDenied,
                other => other,
            })?;
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
        if !self
            .store
            .assignments()
            .insertable(&auth.scope, &assignment)
            .map_err(|e| assignment_repo(e, assignment.id))?
        {
            return Err(ManagementError::CapabilityDenied);
        }
        if self
            .store
            .assignments()
            .exists_for(&conn, &AccessScope::allow_all(), bundle_id, spec.tenant_id)
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?
        {
            return Err(ManagementError::AssignmentExists {
                bundle_id,
                tenant_id: spec.tenant_id,
            });
        }
        Ok((auth, assignment))
    }

    /// Every check [`Self::update_assignment`] runs, the authorisation it
    /// would write under, and whether the assignment is dormant (the update
    /// changes neither of the tenants that decide it); reads only.
    ///
    /// # Errors
    ///
    /// As [`Self::update_assignment`].
    pub(super) async fn plan_update(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<(Authorization, bool), ManagementError> {
        let current = self.prefetch_assignment(assignment_id).await?;
        let auth = match self.assignment_access(ctx, &current).await {
            Ok(auth) => auth,
            Err(err @ ManagementError::NotFound { .. }) => {
                return Err(self.refusal(ctx, &current, err).await?);
            }
            Err(err) => return Err(err),
        };
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        // The update writes under the grant's scope; one that does not admit
        // the row would change nothing.
        match self.coverage(&auth, assignment_id).await? {
            Coverage::Covered => {}
            Coverage::Uncovered => {
                let absent = assignment_not_found(assignment_id);
                return Err(self.refusal(ctx, &current, absent).await?);
            }
            Coverage::Gone => return Err(assignment_not_found(assignment_id)),
        }
        let dormant = self.is_dormant(ctx, &current).await?;
        Ok((auth, dormant))
    }

    /// Every check [`Self::unassign`] runs, and what it would delete; reads
    /// only.
    ///
    /// # Errors
    ///
    /// As [`Self::unassign`].
    pub(super) async fn plan_unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<Withdrawal, ManagementError> {
        let Some(current) = self.find_assignment(assignment_id).await? else {
            return Ok(Withdrawal::Nothing);
        };
        let auth = match self.assignment_access(ctx, &current).await {
            Ok(auth) => auth,
            Err(err) => {
                if let Some(withdrawal) = self.dormant_withdrawal(ctx, &current).await? {
                    return Ok(withdrawal);
                }
                return match err {
                    // Not visible to the caller: as good as absent. One it
                    // sees but may not remove is refused, never reported
                    // withdrawn while it stays in force.
                    ManagementError::NotFound { .. } => {
                        if self.seen(ctx, &current).await?.is_some() {
                            Err(ManagementError::CapabilityDenied)
                        } else {
                            Ok(Withdrawal::Nothing)
                        }
                    }
                    other => Err(other),
                };
            }
        };
        // The caller administers it: a tenant out of reach is a refusal, never
        // a success that leaves the assignment enforcing.
        self.guard_tenant(ctx, assignment_id, current.tenant_id)
            .await?;
        match self.coverage(&auth, assignment_id).await? {
            Coverage::Covered => Ok(Withdrawal::Delete {
                auth,
                assignment: current,
            }),
            // Withdrawn concurrently since it was read: as good as absent.
            Coverage::Gone => Ok(Withdrawal::Nothing),
            // Authorised, but the grant does not cover this row.
            Coverage::Uncovered => self
                .dormant_withdrawal(ctx, &current)
                .await?
                .ok_or(ManagementError::CapabilityDenied),
        }
    }

    // -- Shared steps --------------------------------------------------------

    /// What the caller sees of the assignments (module documentation,
    /// "Visibility"); `None` when neither read nor publish is granted.
    ///
    /// # Errors
    ///
    /// `service_unavailable` when the PDP or the hierarchy cannot answer.
    async fn assignment_view(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Option<AssignmentView>, ManagementError> {
        let read = self.collection_grant(ctx, Capability::Read).await?;
        let publish = self.collection_grant(ctx, Capability::Publish).await?;
        if read.is_none() && publish.is_none() {
            return Ok(None);
        }
        let subtree = self.caller_subtree(ctx).await?;
        let chain = match subtree {
            Some(_) => self.caller_chain(ctx).await?,
            None => None,
        };
        let (tenants, reachable) = subtree
            .as_ref()
            .map(|tree| (tree.tenants(), tree.reachable()))
            .unwrap_or_default();
        // Without the chain an ancestor's live assignment would read as
        // dormant: then none is shown as withdrawable.
        let withdrawable = match (&publish, &chain) {
            (Some(auth), Some(_)) => reachable
                .iter()
                .copied()
                .filter(|&tenant| auth.covers_tenant(tenant))
                .collect(),
            _ => Vec::new(),
        };
        let chain = chain.unwrap_or_default();
        let above: Vec<Uuid> = chain
            .iter()
            .copied()
            .filter(|t| !reachable.contains(t))
            .collect();
        let mut reaching = reachable;
        reaching.extend(above);
        Ok(Some(AssignmentView {
            visibility: AssignmentVisibility {
                read: read.map(|auth| auth.scope),
                subtree: tenants,
                reaching,
                withdrawable,
            },
            subtree,
            chain,
        }))
    }

    /// `capability` over the collection; `None` when not granted.
    async fn collection_grant(
        &self,
        ctx: &SecurityContext,
        capability: Capability,
    ) -> Result<Option<Authorization>, ManagementError> {
        match self
            .authz
            .authorize(ctx, capability, AccessTarget::collection())
            .await
        {
            Ok(auth) => Ok(Some(auth)),
            Err(AuthzError::Denied) => Ok(None),
            Err(err) => Err(error::authz(err, || ManagementError::CapabilityDenied)),
        }
    }

    /// The caller's tenant with all its descendants; `None` for a caller
    /// without a tenant the hierarchy knows.
    async fn caller_subtree(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Option<Subtree>, ManagementError> {
        let from = ctx.subject_tenant_id();
        if from.is_nil() {
            return Ok(None);
        }
        match self.hierarchy.subtree(ctx, TenantId(from)).await {
            Ok(tree) => Ok(Some(tree)),
            Err(PortError::NotFound(_)) => Ok(None),
            Err(err) => Err(error::unavailable(err)),
        }
    }

    /// The caller's tenant and its ancestors with barriers respected; `None`
    /// when the hierarchy does not know it.
    async fn caller_chain(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Option<Vec<Uuid>>, ManagementError> {
        match self
            .hierarchy
            .ancestors(ctx, TenantId(ctx.subject_tenant_id()))
            .await
        {
            Ok(chain) => Ok(Some(chain)),
            Err(PortError::NotFound(_)) => Ok(None),
            Err(err) => Err(error::unavailable(err)),
        }
    }

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

    /// The withdrawal of `current` on behalf of an administrator of its
    /// target tenant, when it is dormant (its owner no longer reaches the
    /// target with barriers respected) and the caller reaches the target and
    /// holds the publish capability there - a grant whose scope covers this
    /// row as content of the target tenant. `None` when any of that does not
    /// hold; the caller then answers as it would without this path.
    ///
    /// # Errors
    ///
    /// `service_unavailable` when the hierarchy or the PDP cannot answer;
    /// `CAPABILITY_DENIED` when even the row-confined scope misses the row.
    async fn dormant_withdrawal(
        &self,
        ctx: &SecurityContext,
        current: &Assignment,
    ) -> Result<Option<Withdrawal>, ManagementError> {
        if !self.is_dormant(ctx, current).await?
            || self.reach(ctx, current.tenant_id).await? != Reach::Reachable
        {
            return Ok(None);
        }
        // A decision for this one prefetched row. A grant can come with
        // constraints (another tenant's content, other rows): it counts only
        // when they admit this row as content of the target tenant.
        let target = AccessTarget::owned_by(current.tenant_id)
            .resource(current.id)
            .prefetched();
        match self.authz.authorize(ctx, Capability::Publish, target).await {
            Ok(auth) if auth.covers(current.tenant_id, current.id.0) => {}
            Ok(_) | Err(AuthzError::Denied) => return Ok(None),
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
        match self.coverage(&auth, current.id).await? {
            Coverage::Covered => Ok(Some(Withdrawal::Delete {
                auth,
                assignment: current.clone(),
            })),
            Coverage::Gone => Ok(Some(Withdrawal::Nothing)),
            Coverage::Uncovered => Err(ManagementError::CapabilityDenied),
        }
    }

    /// What a write under `auth` would find of the assignment `id`, read
    /// without writing.
    async fn coverage(
        &self,
        auth: &Authorization,
        id: AssignmentId,
    ) -> Result<Coverage, ManagementError> {
        let conn = self.conn()?;
        let scoped = self
            .store
            .assignments()
            .get(&conn, &auth.scope, id)
            .await
            .map_err(|e| assignment_repo(e, id))?;
        Ok(match scoped {
            Some(_) => Coverage::Covered,
            None if self.find_assignment(id).await?.is_some() => Coverage::Uncovered,
            None => Coverage::Gone,
        })
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

/// What a caller sees of the assignments, and how to tell a dormant one.
struct AssignmentView {
    /// The rows it sees.
    visibility: AssignmentVisibility,
    /// Its tenant and all descendants, with their barriers.
    subtree: Option<Subtree>,
    /// Its tenant and ancestors, barriers respected.
    chain: Vec<Uuid>,
}

impl AssignmentView {
    /// The SDK view of a visible row: live when its owner reaches its
    /// target with barriers respected. An owner in the caller's subtree does
    /// when no barrier lies between them; an ancestor in the caller's chain
    /// reaches the caller, so it does exactly when the caller does. An owner
    /// on the far side of a barrier (a withdrawable row) never does.
    fn to_sdk(&self, assignment: &Assignment) -> sdk::Assignment {
        let (owner, target) = (assignment.owner_tenant_id, assignment.tenant_id);
        let live = self.subtree.as_ref().is_some_and(|tree| {
            if tree.contains(owner) {
                tree.reaches(owner, target)
            } else {
                self.chain.contains(&owner) && tree.reaches(tree.root(), target)
            }
        });
        assignment_to_sdk(assignment, !live)
    }
}

/// What a write under some scope would find of a stored assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coverage {
    /// The row, within the scope.
    Covered,
    /// The row exists outside the scope.
    Uncovered,
    /// The row no longer exists.
    Gone,
}

/// What [`ManagementService::plan_unassign`] decided.
#[domain_model]
#[derive(Debug)]
pub(super) enum Withdrawal {
    /// Nothing to withdraw: absent, or not visible to the caller.
    Nothing,
    /// Delete `assignment` under `auth`.
    Delete {
        /// The scope the delete runs under.
        auth: Authorization,
        /// The assignment, as read.
        assignment: Assignment,
    },
}

/// The record of an assignment change: who, what, where.
fn log_change(change: &'static str, ctx: &SecurityContext, assignment: &Assignment) {
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

/// The SDK view of an assignment; `dormant` as the caller determined it.
#[must_use]
pub fn assignment_to_sdk(assignment: &Assignment, dormant: bool) -> sdk::Assignment {
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
        dormant,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "assignments_tests.rs"]
mod assignments_tests;
