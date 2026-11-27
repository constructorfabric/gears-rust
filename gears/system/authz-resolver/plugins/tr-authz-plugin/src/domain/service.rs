// Created: 2026-04-23 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech

//! Service implementation for the TR `AuthZ` resolver plugin.
//!
//! Implements the 8 access-check rules (R1–R8) from the tenant-based access
//! algorithm: each evaluation maps to exactly one rule, depending on three
//! axes read from the request:
//!
//! - **single-resource vs list** — decided by `resource.id`.
//!   `Some(_)` → single (GET/UPDATE/DELETE); `None` → list/create.
//! - **explicit target tenant** — `context.tenant_context.root_id`
//!   (`Some` / `None`).
//! - **scope mode** — `context.tenant_context.mode`
//!   (`RootOnly` / `Subtree`; default `Subtree`).
//!
//! See `docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md` for the full matrix
//! and per-rule HTTP examples.

use std::sync::Arc;

use authz_resolver_sdk::{
    AuthZResolverError, BarrierMode as AuthzBarrierMode, Capability, Constraint, DenyReason,
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InGroupPredicate,
    InGroupSubtreePredicate, InPredicate, Predicate, TenantMode,
};
use tenant_resolver_sdk::{
    BarrierMode, GetDescendantsOptions, IsAncestorOptions, TenantId, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use toolkit_security::{SecurityContext, pep_properties};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// TR-based `AuthZ` resolver service.
///
/// Resolves tenant hierarchy via `TenantResolverClient`.
#[toolkit_macros::domain_model]
pub struct Service {
    tr: Arc<dyn TenantResolverClient>,
}

impl Service {
    pub fn new(tr: Arc<dyn TenantResolverClient>) -> Self {
        Self { tr }
    }

    /// Evaluate an authorization request.
    ///
    /// Branches the request into one of 8 rules (R1–R8) by
    /// `resource.id.is_some()` × `tenant_context.root_id.is_some()` × `mode`.
    /// On any failed access check, unknown tenant, or missing or malformed
    /// field — returns a coded `deny` (fail-closed).
    ///
    /// # Errors
    ///
    /// A Tenant Resolver failure other than an unknown tenant
    /// (`ServiceUnavailable` when transient, `Internal` otherwise): a technical
    /// failure is never returned as a denial.
    #[allow(clippy::cognitive_complexity)]
    pub async fn evaluate(
        &self,
        request: &EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        info!(
            action = %request.action.name,
            resource_type = %request.resource.resource_type,
            "tr-authz: evaluate called"
        );

        // Subject tenant is required in every rule (R3/R4/R7/R8 use it directly;
        // R1/R2/R5/R6 use it inside `is_in_subtree`).
        let Some(subject_tid) = Self::read_uuid(&request.subject.properties, "tenant_id") else {
            warn!("tr-authz: subject tenant_id missing or unparseable -- deny");
            return Ok(Self::deny(DenyReason::INVALID_REQUEST));
        };
        if subject_tid == Uuid::nil() {
            warn!("tr-authz: subject tenant_id is nil -- deny");
            return Ok(Self::deny(DenyReason::INVALID_REQUEST));
        }

        let tc = request.context.tenant_context.as_ref();
        let root_id = tc.and_then(|t| t.root_id);
        let mode = tc.map(|t| t.mode.clone()).unwrap_or_default();
        let barrier_mode =
            Self::tr_barrier_mode(tc.map_or(AuthzBarrierMode::default(), |t| t.barrier_mode));

        // Parse caller-supplied tenant_status filter once, up-front. Any
        // unknown status string fails closed (deny) — silently dropping it
        // would widen the visible-subtree set in R6/R8.
        let tenant_statuses = match tc.and_then(|t| t.tenant_status.as_deref()) {
            None => Vec::new(),
            Some(strs) => match Self::parse_tenant_statuses(strs) {
                Ok(v) => v,
                Err(bad) => {
                    warn!(%bad, "tr-authz: unknown tenant_status value -- deny");
                    return Ok(Self::deny(DenyReason::INVALID_REQUEST));
                }
            },
        };

        // tr-authz is a trusted plugin by design and MUST NOT propagate caller
        // scope into TR calls: the access-check rules (R1/R2/R5/R6) walk from
        // `subject` toward `root_id`, which routinely lies outside the caller's
        // visibility — a scope-limited view would hide legitimate ancestors and
        // yield wrong allow/deny decisions. The plugin must keep its full-tree
        // visibility either way.
        //
        // TODO(https://github.com/constructorfabric/gears-rust/issues/1597):
        // once the platform S2S authentication subsystem and the gRPC + mTLS
        // transport land, replace `SecurityContext::anonymous()` here with the
        // S2S-issued service context identifying this caller as
        // `tr-authz-plugin`. Anonymous is safe today (in-process trust boundary
        // between ToolKit gears) but unsafe over a network boundary — there
        // is no cryptographic identity on the wire.
        let ctx = SecurityContext::anonymous();

        let mut response = if request.resource.id.is_some() {
            // Single-resource: owner_tenant_id is mandatory (PEP must prefetch it).
            let Some(owner_tid) = Self::read_uuid(
                &request.resource.properties,
                pep_properties::OWNER_TENANT_ID,
            ) else {
                warn!(
                    "tr-authz: single-resource request missing owner_tenant_id in properties -- deny"
                );
                return Ok(Self::deny(DenyReason::INVALID_REQUEST));
            };
            self.evaluate_single(&ctx, subject_tid, owner_tid, root_id, &mode, barrier_mode)
                .await?
        } else {
            self.evaluate_list(
                &ctx,
                subject_tid,
                root_id,
                &mode,
                barrier_mode,
                &tenant_statuses,
            )
            .await?
        };

        // Group predicates are orthogonal to tenant scoping — append only on
        // allow. If the group property is present but any of its UUIDs are
        // malformed, the group predicate cannot be compiled; fail-closed to
        // avoid silently widening scope to tenant-wide access.
        if response.decision
            && Self::append_group_predicates(
                &mut response,
                &request.resource.properties,
                &request.context.capabilities,
            )
            .is_err()
        {
            warn!("tr-authz: malformed group scoping properties -- deny");
            return Ok(Self::deny(DenyReason::INVALID_REQUEST));
        }

        // A decision-only CREATE (`require_constraints = false`, per the request
        // model) on no particular resource keeps the tenant/hierarchy decision
        // above but drops the owner predicate the PEP declared it cannot
        // compile. Any other action, a single-resource request, and any request
        // naming a group selector keep their constraints and fail closed as
        // before: a PEP that sends these flags on a list or a GET still gets
        // its row fence.
        if response.decision
            && request.action.name == "create"
            && request.resource.id.is_none()
            && !request.context.require_constraints
            && request.context.supported_properties.is_empty()
            && !request.resource.properties.contains_key("group_ids")
            && !request
                .resource
                .properties
                .contains_key("ancestor_group_ids")
        {
            response.context.constraints.clear();
        }

        Ok(response)
    }

    // ── Single-resource branches (R1–R4) ──────────────────────────────────

    #[allow(clippy::cognitive_complexity)]
    async fn evaluate_single(
        &self,
        ctx: &SecurityContext,
        subject: Uuid,
        owner: Uuid,
        root_id: Option<Uuid>,
        mode: &TenantMode,
        barrier_mode: BarrierMode,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        match (root_id, mode) {
            (Some(root), TenantMode::RootOnly) => {
                // R1: GET /tasks/{id}?tenant=t2&tenant_mode=root_only
                if owner != root {
                    warn!(%owner, %root, "R1: owner_tenant_id != root_id -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                if !self.is_in_subtree(ctx, subject, root, barrier_mode).await? {
                    warn!(%subject, %root, "R1: subject is not an ancestor of root_id -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                debug!(rule = "R1", %owner, "tr-authz: allow");
                Ok(Self::allow_eq(owner))
            }
            (Some(root), TenantMode::Subtree) => {
                // R2: GET /tasks/{id}?tenant=t2
                if !self.is_in_subtree(ctx, root, owner, barrier_mode).await? {
                    warn!(%owner, %root, "R2: owner is not in root_id subtree -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                if !self.is_in_subtree(ctx, subject, root, barrier_mode).await? {
                    warn!(%subject, %root, "R2: subject is not an ancestor of root_id -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                debug!(rule = "R2", %owner, "tr-authz: allow");
                Ok(Self::allow_eq(owner))
            }
            (None, TenantMode::RootOnly) => {
                // R3: GET /tasks/{id}?tenant_mode=root_only
                if owner != subject {
                    warn!(%owner, %subject, "R3: owner_tenant_id != subject tenant -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                debug!(rule = "R3", %owner, "tr-authz: allow");
                Ok(Self::allow_eq(owner))
            }
            (None, TenantMode::Subtree) => {
                // R4: GET /tasks/{id}
                if !self
                    .is_in_subtree(ctx, subject, owner, barrier_mode)
                    .await?
                {
                    warn!(%owner, %subject, "R4: owner is not in subject subtree -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                debug!(rule = "R4", %owner, "tr-authz: allow");
                Ok(Self::allow_eq(owner))
            }
        }
    }

    // ── List / CREATE branches (R5–R8) ────────────────────────────────────

    #[allow(clippy::cognitive_complexity)]
    async fn evaluate_list(
        &self,
        ctx: &SecurityContext,
        subject: Uuid,
        root_id: Option<Uuid>,
        mode: &TenantMode,
        barrier_mode: BarrierMode,
        tenant_statuses: &[TenantStatus],
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        match (root_id, mode) {
            (Some(root), TenantMode::RootOnly) => {
                // R5: GET /tasks?tenant=t2&tenant_mode=root_only
                // Subject must be (reflexive) ancestor of root_id.
                if !self.is_in_subtree(ctx, subject, root, barrier_mode).await? {
                    warn!(%subject, %root, "R5: subject is not an ancestor of root_id -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                debug!(rule = "R5", %root, "tr-authz: allow");
                Ok(Self::allow_eq(root))
            }
            (Some(root), TenantMode::Subtree) => {
                // R6: GET /tasks?tenant=t2
                // Subject must be (reflexive) ancestor of root_id.
                if !self.is_in_subtree(ctx, subject, root, barrier_mode).await? {
                    warn!(%subject, %root, "R6: subject is not an ancestor of root_id -- deny");
                    return Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS));
                }
                match self
                    .resolve_subtree(ctx, root, barrier_mode, tenant_statuses)
                    .await?
                {
                    Some(ids) if !ids.is_empty() => {
                        debug!(rule = "R6", %root, visible = ids.len(), "tr-authz: allow");
                        Ok(Self::allow_in(ids))
                    }
                    _ => {
                        warn!(%root, "R6: tenant not found or no visible tenants -- deny");
                        Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS))
                    }
                }
            }
            (None, TenantMode::RootOnly) => {
                // R7: GET /tasks?tenant_mode=root_only
                debug!(rule = "R7", %subject, "tr-authz: allow");
                Ok(Self::allow_eq(subject))
            }
            (None, TenantMode::Subtree) => {
                // R8: GET /tasks
                match self
                    .resolve_subtree(ctx, subject, barrier_mode, tenant_statuses)
                    .await?
                {
                    Some(ids) if !ids.is_empty() => {
                        debug!(rule = "R8", %subject, visible = ids.len(), "tr-authz: allow");
                        Ok(Self::allow_in(ids))
                    }
                    _ => {
                        warn!(%subject, "R8: tenant not found or no visible tenants -- deny");
                        Ok(Self::deny(DenyReason::INSUFFICIENT_PERMISSIONS))
                    }
                }
            }
        }
    }

    // ── TR helpers ────────────────────────────────────────────────────────

    /// Reflexive "candidate is in the closed subtree rooted at `anchor`".
    /// An unknown tenant is `false` (a policy answer); any other TR failure is
    /// an error, never a denial: a technical failure is not a decision.
    async fn is_in_subtree(
        &self,
        ctx: &SecurityContext,
        anchor: Uuid,
        candidate: Uuid,
        barrier_mode: BarrierMode,
    ) -> Result<bool, AuthZResolverError> {
        if anchor == candidate {
            return Ok(true);
        }
        match self
            .tr
            .is_ancestor(
                ctx,
                TenantId(anchor),
                TenantId(candidate),
                &IsAncestorOptions { barrier_mode },
            )
            .await
        {
            Ok(v) => Ok(v),
            Err(TenantResolverError::TenantNotFound { .. }) => Ok(false),
            Err(e) => Err(tr_failure("is_ancestor", &e)),
        }
    }

    /// Resolve the closed subtree (root + descendants) as UUIDs.
    ///
    /// `tenant_statuses` filters descendants by status (empty = all). Per the
    /// TR SDK contract (`GetDescendantsOptions::status`), the filter does not
    /// apply to the starting tenant itself. `None` when the tenant is unknown
    /// (a policy answer); any other TR failure is an error.
    async fn resolve_subtree(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        barrier_mode: BarrierMode,
        tenant_statuses: &[TenantStatus],
    ) -> Result<Option<Vec<Uuid>>, AuthZResolverError> {
        let result = self
            .tr
            .get_descendants(
                ctx,
                TenantId(tenant_id),
                &GetDescendantsOptions {
                    status: tenant_statuses.to_vec(),
                    barrier_mode,
                    max_depth: None,
                },
            )
            .await;
        let response = match result {
            Ok(response) => response,
            Err(TenantResolverError::TenantNotFound { .. }) => return Ok(None),
            Err(e) => return Err(tr_failure("get_descendants", &e)),
        };

        let mut visible = Vec::with_capacity(response.descendants.len() + 1);
        visible.push(response.tenant.id.0);
        visible.extend(response.descendants.iter().map(|t| t.id.0));
        Ok(Some(visible))
    }

    // ── Response builders ────────────────────────────────────────────────

    fn allow_eq(tenant_id: Uuid) -> EvaluationResponse {
        Self::allow(vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [tenant_id],
        ))])
    }

    fn allow_in(tenant_ids: Vec<Uuid>) -> EvaluationResponse {
        Self::allow(vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            tenant_ids,
        ))])
    }

    fn allow(predicates: Vec<Predicate>) -> EvaluationResponse {
        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                ..Default::default()
            },
        }
    }

    /// A denial carrying `error_code`: malformed or missing request input is
    /// [`DenyReason::INVALID_REQUEST`], a policy refusal
    /// [`DenyReason::INSUFFICIENT_PERMISSIONS`]. A batch denial must carry one.
    fn deny(error_code: &str) -> EvaluationResponse {
        EvaluationResponse::deny(DenyReason::new(error_code))
    }

    // ── Group predicates (orthogonal to tenant) ──────────────────────────

    /// Returns `Err(())` when a group scoping property is present but cannot
    /// be parsed as a full `Vec<Uuid>` (e.g. not an array, or contains a
    /// non-UUID string). Caller maps that to `deny` (fail-closed). Missing
    /// properties are `Ok(())`, but a present empty array is a deny: its set
    /// semantics are "match nothing", not "remove group filtering". A native
    /// predicate that the PEP did not advertise is also rejected rather than
    /// silently widening the response to tenant-only access.
    fn append_group_predicates(
        response: &mut EvaluationResponse,
        props: &std::collections::HashMap<String, serde_json::Value>,
        capabilities: &[Capability],
    ) -> Result<(), ()> {
        let Some(Constraint { predicates }) = response.context.constraints.get_mut(0) else {
            return Ok(());
        };
        if let Some(group_ids) = props.get("group_ids") {
            let ids = Self::parse_uuid_array(group_ids).ok_or(())?;
            if ids.is_empty() || !capabilities.contains(&Capability::GroupMembership) {
                return Err(());
            }
            predicates.push(Predicate::InGroup(InGroupPredicate::new("id", ids)));
        }
        if let Some(ancestor_ids) = props.get("ancestor_group_ids") {
            let ids = Self::parse_uuid_array(ancestor_ids).ok_or(())?;
            if ids.is_empty()
                || !capabilities.contains(&Capability::GroupMembership)
                || !capabilities.contains(&Capability::GroupHierarchy)
            {
                return Err(());
            }
            predicates.push(Predicate::InGroupSubtree(InGroupSubtreePredicate::new(
                "id", ids,
            )));
        }
        Ok(())
    }

    // ── Parsing helpers ──────────────────────────────────────────────────

    fn read_uuid(
        props: &std::collections::HashMap<String, serde_json::Value>,
        key: &str,
    ) -> Option<Uuid> {
        props
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    }

    /// Strict array-of-UUID parse: returns `None` when the JSON value is not
    /// an array, OR when any element is not a valid UUID string. Callers treat
    /// `None` as a hard error (fail-closed) rather than silently dropping the
    /// bad entries, which would widen the resulting access scope.
    fn parse_uuid_array(value: &serde_json::Value) -> Option<Vec<Uuid>> {
        let arr = value.as_array()?;
        arr.iter()
            .map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
            .collect()
    }

    /// Map caller-supplied `tenant_status` strings to TR SDK `TenantStatus`.
    /// Returns the first unrecognized value on failure so the caller can
    /// fail-closed with a diagnostic — silently dropping unknowns would widen
    /// the status filter to "all statuses" and leak suspended/deleted tenants.
    ///
    /// Accepted values match the SDK's `#[serde(rename_all = "snake_case")]`
    /// representation of `TenantStatus`: `active`, `suspended`, `deleted`.
    fn parse_tenant_statuses(statuses: &[String]) -> Result<Vec<TenantStatus>, String> {
        statuses
            .iter()
            .map(|s| match s.as_str() {
                "active" => Ok(TenantStatus::Active),
                "suspended" => Ok(TenantStatus::Suspended),
                "deleted" => Ok(TenantStatus::Deleted),
                other => Err(other.to_owned()),
            })
            .collect()
    }

    fn tr_barrier_mode(mode: AuthzBarrierMode) -> BarrierMode {
        match mode {
            AuthzBarrierMode::Respect => BarrierMode::Respect,
            AuthzBarrierMode::Ignore => BarrierMode::Ignore,
        }
    }
}

/// A Tenant Resolver failure other than an unknown tenant: unavailable or
/// broken infrastructure, never a policy answer. Transient conditions are
/// retryable `ServiceUnavailable`; the rest are `Internal`.
fn tr_failure(call: &str, e: &TenantResolverError) -> AuthZResolverError {
    warn!(error = %e, call, "tr-authz: tenant resolver failure -- evaluation fails");
    match e {
        TenantResolverError::ServiceUnavailable(_) | TenantResolverError::NoPluginAvailable => {
            AuthZResolverError::ServiceUnavailable(format!("tenant resolver {call}: {e}"))
        }
        _ => AuthZResolverError::Internal(format!("tenant resolver {call}: {e}")),
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
