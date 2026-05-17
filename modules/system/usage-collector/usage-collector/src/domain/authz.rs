//! Authorization algorithm for query endpoints: `authorize_and_compile_scope`.
//!
//! Implements the PDP call, fail-closed error handling, and OR-of-ANDs scope
//! compilation for the aggregated and raw query endpoints.
//!
//! Resource type, action, and property constants live in
//! [`usage_collector_sdk::authz`]. This module re-uses those constants so the
//! same `USAGE_RECORD` resource type is shared with the emitter (`CREATE`) and
//! the collector gateway (`LIST`).

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::EnforcerError;
use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
use modkit_security::{AccessScope, SecurityContext};
use tracing::error;
use usage_collector_sdk::{UsageCollectorError, UsageRecordError};

/// Call the PDP, compile constraints into an [`AccessScope`], and return it.
///
/// Implements the authorize-and-compile-scope algorithm
/// (`cpt-cf-usage-collector-algo-query-api-authz-delegate`):
///
/// 1. Builds an `AccessRequest` with `require_constraints(true)`.
///    `BarrierMode::Respect` is the `AccessRequest` default and is preserved.
/// 2. Calls `PolicyEnforcer::access_scope_with` — the enforcer internally compiles
///    the PDP constraints into an `AccessScope`, preserving the OR-of-ANDs
///    structure without flattening.
/// 3. Maps `Err(Denied)` → `Err(PermissionDenied)` immediately
///    (`inst-authz-3` / `inst-authz-3a`).
/// 4. Maps any other PDP error (`EvaluationFailed`, `CompileFailed`) →
///    `Err(PermissionDenied)` (fail-closed). Logs at `ERROR` level with the
///    caller's subject ID as correlation; never logs raw PDP error details or
///    PII (`inst-authz-3b`).
/// 5. Returns `Ok(scope)` on success (`inst-authz-4` / `inst-authz-5`).
///
/// # Errors
///
/// Returns [`UsageCollectorError::PermissionDenied`] on any PDP error (Denied
/// or non-Denied). No allow-all path exists for any PDP error condition.
// @cpt-algo:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1
pub async fn authorize_and_compile_scope(
    ctx: &SecurityContext,
    authz: Arc<dyn AuthZResolverClient>,
    resource_type: &ResourceType,
    action: &str,
) -> Result<AccessScope, UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-1
    let request = AccessRequest::new().require_constraints(true);
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-1

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-2
    let result = PolicyEnforcer::new(authz)
        .access_scope_with(ctx, resource_type, action, None, &request)
        .await;
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-2

    match result {
        // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-4
        // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-5
        Ok(scope) => Ok(scope),
        // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-5
        // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-4

        // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3
        Err(EnforcerError::Denied { .. }) => {
            // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3a
            Err(UsageRecordError::permission_denied()
                .with_reason("AUTHORIZATION_DENIED")
                .create())
            // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3

        // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
        Err(e) => {
            // Inner reason comes from the PDP itself (inside our trust
            // boundary), so unlike plugin context it is safe to surface in
            // logs and is required for operator triage — without it a 503
            // burst looks like "the PDP errored" with no way to tell which
            // backend issue is firing. PII / SQL fragments cannot reach this
            // path because `EnforcerError::EvaluationFailed` / `CompileFailed`
            // carry strings produced by the PDP, not by the storage plugin.
            error!(
                subject_id = %ctx.subject_id(),
                pdp_error_variant = pdp_error_variant_name(&e),
                pdp_error_reason = %PdpErrorReason(&e),
                "PDP infrastructure error (non-Denied); access denied (fail-closed)",
            );
            Err(UsageRecordError::permission_denied()
                .with_reason("AUTHORIZATION_DENIED")
                .create())
        } // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    }
}

/// Returns a static variant name string for the given [`EnforcerError`].
///
/// Used for structured logging — never includes raw error details or PII.
fn pdp_error_variant_name(e: &EnforcerError) -> &'static str {
    match e {
        EnforcerError::Denied { .. } => "Denied",
        EnforcerError::EvaluationFailed(_) => "EvaluationFailed",
        EnforcerError::CompileFailed(_) => "CompileFailed",
    }
}

/// `Display` wrapper that renders the inner reason carried by a non-Denied
/// [`EnforcerError`].
///
/// The `Denied` variant has structured fields and is handled separately; for
/// `EvaluationFailed` and `CompileFailed` the inner cause is produced inside
/// the PDP trust boundary, so it is safe to log for operator triage.
///
/// # Trust contract
///
/// Logging the inner `Display` output assumes a PDP-side invariant: the
/// `AuthZResolverError` and `ConstraintCompileError` `Display` impls MUST NOT
/// echo caller-controlled input (subject identifiers, raw resource property
/// values, request-body fragments, …). Their messages should describe the
/// PDP-side failure class (RPC transport failure, missing constraint shape,
/// unsupported barrier mode, …), not the value that triggered it. If a future
/// `EnforcerError` variant adds a payload that could surface caller input via
/// its `Display`, this wrapper MUST be narrowed to the variant tag only — see
/// `pdp_error_variant_name` above, which is unconditionally safe.
struct PdpErrorReason<'a>(&'a EnforcerError);

impl std::fmt::Display for PdpErrorReason<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            EnforcerError::Denied { .. } => Ok(()),
            EnforcerError::EvaluationFailed(inner) => std::fmt::Display::fmt(inner, f),
            EnforcerError::CompileFailed(inner) => std::fmt::Display::fmt(inner, f),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "authz_tests.rs"]
mod authz_tests;
