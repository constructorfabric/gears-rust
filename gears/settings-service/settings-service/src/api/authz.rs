// @cpt-dod:cpt-cf-settings-service-dod-category-management-authorization:p1
//! The authorization enforcement point.
//!
//! Every handler that touches a settings resource obtains its [`AccessScope`]
//! here first. The scope is not advisory: it is the query and visibility
//! predicate the handler applies, so a caller entitled to part of the tree
//! cannot read past it by asking differently.
//!
//! # Failing closed is structural, not a policy this module applies
//!
//! [`PolicyEnforcer::access_scope`] returns `Result<AccessScope, EnforcerError>`
//! — an explicit deny, an unreachable decision point, and an unusable set of
//! constraints are all `Err`. There is no path that yields a scope without a
//! positive decision, so a handler cannot proceed on an unknown verdict by
//! forgetting a check: it has nothing to proceed *with*.
//!
//! What this module adds is the projection of that failure into the gear's own
//! error vocabulary, and the guarantee that **every** failure shape denies. A
//! new `EnforcerError` variant must be handled here or the match stops
//! compiling.

use authz_resolver_sdk::EnforcerError;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::metric_labels::AuthzReason;
use crate::domain::ports::SecurityMetrics;

/// The settings resources authorization decisions are made against.
///
/// These are the GTS type ids the policy decision point knows, so they are the
/// vocabulary a policy is written in — not an internal naming choice.
pub mod resource {
    use super::ResourceType;

    /// A setting declaration — the record of what a setting *is*.
    pub const DECLARATION: ResourceType = ResourceType::from_static(
        toolkit_gts::gts_id!("cf.core.settings.declaration.v1~"),
        &[],
    );

    /// A stored setting value at some scope.
    pub const VALUE: ResourceType =
        ResourceType::from_static(toolkit_gts::gts_id!("cf.core.settings.value.v1~"), &[]);

    /// A settings category.
    pub const CATEGORY: ResourceType =
        ResourceType::from_static(toolkit_gts::gts_id!("cf.core.settings.category.v1~"), &[]);
}

/// The actions the enforcement point asks a decision for — the `operation`
/// a refusal is counted under (`settings_authz_denial_total`).
///
/// The unmasked-read entitlement is not one of them: it is a probe answered
/// by masking, asked through [`may_read_unmasked`] and never counted, and the
/// type keeps it off the counted path.
pub use crate::domain::metric_labels::Operation;

/// The entitlement that unmasks `pii` values on the administrative surface.
/// Private: it is asked only through [`may_read_unmasked`].
const READ_UNMASKED: &str = "read_unmasked";

/// Project an enforcement failure into the gear's error vocabulary.
///
/// Every known outcome denies. `EnforcerError` is `#[non_exhaustive]`: an
/// outcome class added upstream is an internal failure ([`DomainError::Internal`],
/// HTTP 500, no detail on the wire), never a default that might allow.
///
/// The three known outcomes produce the **same** [`DomainError::Unauthorized`],
/// which carries no identifier and no existence hint. A caller cannot learn from the
/// response whether the policy point was down, whether a policy explicitly
/// denied them, or whether the resource exists at all — and it should not: each
/// of those is information about a resource it has not been granted.
#[must_use]
pub fn deny(resource: &ResourceType, err: &EnforcerError) -> DomainError {
    match err {
        // The decision point said no, or permitted a scope that matches nothing.
        EnforcerError::AccessDenied(_)
        // The decision point could not be reached. Step 4 of the enforcement
        // algorithm: fail closed rather than proceed on an unknown verdict.
        | EnforcerError::EvaluationFailed { .. }
        // A decision arrived but its constraints could not be compiled into a
        // scope. Allowing here would mean applying no predicate at all, which
        // is broader than any decision the policy point could have returned.
        | EnforcerError::CompileFailed { .. } => DomainError::Unauthorized {
            resource: resource_kind(resource),
        },
        // An unknown outcome class: stop with an internal failure; its detail
        // stays in the in-process diagnostic.
        unknown => DomainError::Internal {
            diagnostic: format!("authorization failed with an unknown outcome: {unknown}"),
        },
    }
}

/// Why the enforcement point refused, as the `reason` label spells it; `None`
/// for an outcome class this gear does not know. `EnforcerError` is
/// `#[non_exhaustive]`, and an unknown outcome is an internal failure
/// ([`deny`]), not a refusal, so it is not counted as one.
fn refusal_reason(err: &EnforcerError) -> Option<AuthzReason> {
    match err {
        EnforcerError::AccessDenied(_) => Some(AuthzReason::Denied),
        EnforcerError::EvaluationFailed { .. } => Some(AuthzReason::Unavailable),
        EnforcerError::CompileFailed { .. } => Some(AuthzReason::Constraints),
        _ => None,
    }
}

/// The GTS type id a resource is known by, as a `'static` string.
///
/// `ResourceType::name` borrows; the error variant holds `&'static str` so it
/// cannot carry a caller-supplied value even by accident.
fn resource_kind(resource: &ResourceType) -> &'static str {
    match resource.name() {
        n if n == settings_service_sdk::gts::CATEGORY_SCHEMA => {
            settings_service_sdk::gts::CATEGORY_SCHEMA
        }
        n if n == settings_service_sdk::gts::VALUE_SCHEMA => {
            settings_service_sdk::gts::VALUE_SCHEMA
        }
        _ => settings_service_sdk::gts::DECLARATION_SCHEMA,
    }
}

/// Obtain the caller's access scope for an operation on a settings resource.
///
/// A refusal fails the request and is counted on `metrics`.
///
/// # Errors
///
/// [`DomainError::Unauthorized`] for every enforcement failure — see [`deny`].
pub async fn access_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    metrics: &dyn SecurityMetrics,
    ctx: &toolkit_security::SecurityContext,
    resource: &ResourceType,
    operation: Operation,
    resource_id: Option<uuid::Uuid>,
) -> Result<AccessScope, DomainError> {
    enforce(
        enforcer,
        ctx,
        resource,
        operation.as_str(),
        Some((metrics, operation)),
        resource_id,
    )
    .await
}

/// The same decision as [`access_scope`], for a probe whose refusal does not
/// fail the request: the category tallies, answered by omitting the counts.
/// The scope, or `None` when the decision is a known refusal
/// ([`DomainError::Unauthorized`]); any other failure, such as the internal
/// failure an unknown enforcer outcome maps to, propagates and never reads as a
/// refusal. Nothing is counted.
///
/// # Errors
///
/// Every enforcement failure other than [`DomainError::Unauthorized`].
pub async fn optional_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &toolkit_security::SecurityContext,
    resource: &ResourceType,
    operation: Operation,
) -> Result<Option<AccessScope>, DomainError> {
    optional(enforce(enforcer, ctx, resource, operation.as_str(), None, None).await)
}

/// A known refusal is an absent scope; any other failure stays an error.
fn optional(
    decision: Result<AccessScope, DomainError>,
) -> Result<Option<AccessScope>, DomainError> {
    match decision {
        Ok(scope) => Ok(Some(scope)),
        Err(DomainError::Unauthorized { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Whether the caller may see `pii` values unmasked: a separate decision on
/// the value resource, answered by masking when it is a known refusal. A probe,
/// so nothing is counted; any other failure is an error, never a mask.
///
/// # Errors
///
/// Every enforcement failure other than [`DomainError::Unauthorized`].
pub async fn may_read_unmasked(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &toolkit_security::SecurityContext,
) -> Result<bool, DomainError> {
    Ok(
        optional(enforce(enforcer, ctx, &resource::VALUE, READ_UNMASKED, None, None).await)?
            .is_some(),
    )
}

/// The one decision behind [`access_scope`], [`optional_scope`] and
/// [`may_read_unmasked`]: `counted` carries the metrics port and the typed
/// operation a refusal is counted under, and is `None` for a probe, whose
/// action is therefore never a label.
async fn enforce(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &toolkit_security::SecurityContext,
    resource: &ResourceType,
    action: &'static str,
    counted: Option<(&dyn SecurityMetrics, Operation)>,
    resource_id: Option<uuid::Uuid>,
) -> Result<AccessScope, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-3
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-8
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-9
    // `require_constraints(false)`, for every resource here, on purpose.
    //
    // None of the three resources declares a PEP property (`resource::*` above
    // are all `&[]`), so there is nothing a scope constraint could clamp to.
    // Under the default the compiler fails such a request closed --
    // `ConstraintsRequiredButAbsent` -- and the static plugin logs exactly
    // that: "PEP requires constraints but declares none of the properties this
    // plugin constrains". Category and declaration are platform-global rows
    // with no tenant column, which is the whole story for them.
    //
    // A value row does carry `tenant_id`, and it still must not be clamped
    // here. Resolving a `cascading` or `global` setting reads the rows of the
    // caller's *ancestors* -- tenants outside any subtree a policy would clamp
    // the caller to -- so a PDP constraint on `tenant_id` would break
    // resolution rather than protect it (the resolver reads the chain under
    // `AccessScope::allow_all()` for exactly this reason). The tenant boundary
    // for values is enforced by the write and read gates instead:
    // `ValueWriter::gate` refuses a target outside the caller's subtree, a
    // standalone descendant, a tenant-scoped write to a `global` setting, and
    // a caller whose own access is not overridable; the read surface applies
    // the same subtree rule before it resolves. The domain-affinity filter this
    // scope also feeds cannot receive a constraint on a resource that declares
    // no domain property, so nothing is widened by leaving it unconstrained.
    //
    // The decision itself is still the PDP's: a deny is still a deny.
    let scope = enforcer
        .access_scope_with(
            ctx,
            resource,
            action,
            resource_id,
            &AccessRequest::new().require_constraints(false),
        )
        .await
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-4
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-5
        .map_err(|err| {
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-10
            // Counted before the refusal is collapsed into the one answer
            // every shape gets: the reason is the operator's, never the
            // caller's.
            if let (Some((metrics, operation)), Some(reason)) = (counted, refusal_reason(&err)) {
                metrics.authz_denial(operation, reason);
            }
            // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-10
            deny(resource, &err)
        })?;
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-5
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-4
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-9
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-8
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-3
    Ok(scope)
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
