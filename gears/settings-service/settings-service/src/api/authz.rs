// Created: 2026-08-13 by Constructor Tech
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
use authz_resolver_sdk::pep::ResourceType;
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;

/// The settings resources authorization decisions are made against.
///
/// These are the GTS type ids the policy decision point knows, so they are the
/// vocabulary a policy is written in — not an internal naming choice.
pub mod resource {
    use super::ResourceType;

    /// A setting declaration — the record of what a setting *is*.
    pub const DECLARATION: ResourceType =
        ResourceType::from_static("gts.cf.toolkit.settings.declaration.v1~", &[]);

    /// A stored setting value at some scope.
    pub const VALUE: ResourceType =
        ResourceType::from_static("gts.cf.toolkit.settings.value.v1~", &[]);

    /// A settings category.
    pub const CATEGORY: ResourceType =
        ResourceType::from_static("gts.cf.toolkit.settings.category.v1~", &[]);
}

/// Project an enforcement failure into the gear's error vocabulary.
///
/// Every variant denies. The mapping is exhaustive on purpose: a variant added
/// upstream breaks this match rather than falling through to a default that
/// might allow.
///
/// All three denials produce the **same** [`DomainError::Unauthorized`], which
/// carries no identifier and no existence hint. A caller cannot learn from the
/// response whether the policy point was down, whether a policy explicitly
/// denied them, or whether the resource exists at all — and it should not: each
/// of those is information about a resource it has not been granted.
#[must_use]
pub fn deny(err: &EnforcerError) -> DomainError {
    match err {
        // The decision point said no.
        EnforcerError::Denied { .. }
        // The decision point could not be reached. Step 4 of the enforcement
        // algorithm: fail closed rather than proceed on an unknown verdict.
        | EnforcerError::EvaluationFailed { .. }
        // A decision arrived but its constraints could not be compiled into a
        // scope. Allowing here would mean applying no predicate at all, which
        // is broader than any decision the policy point could have returned.
        | EnforcerError::CompileFailed { .. } => DomainError::Unauthorized,
    }
}

/// Obtain the caller's access scope for an action on a settings resource.
///
/// # Errors
///
/// [`DomainError::Unauthorized`] for every enforcement failure — see [`deny`].
pub async fn access_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &toolkit_security::SecurityContext,
    resource: &ResourceType,
    action: &str,
    resource_id: Option<uuid::Uuid>,
) -> Result<AccessScope, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-3
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-8
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-9
    let scope = enforcer
        .access_scope(ctx, resource, action, resource_id)
        .await
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-4
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-5
        .map_err(|err| deny(&err))?;
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
