//! Authorisation of the management surface.
//!
//! [`ManagementAuthorizer`] asks the platform policy enforcer for exactly one
//! capability of the SDK permission catalog per call - the resource type and
//! action come from [`Capability`] itself, so the catalog and the check can
//! never drift apart - against the caller's own [`SecurityContext`]. No
//! capability implies another, and nothing here writes an entitlement.
//!
//! The content surfaces declare barrier handling explicitly
//! ([`BarrierMode::Respect`]) rather than inheriting the platform default.

use authz_resolver_sdk::{AccessRequest, BarrierMode, EnforcerError, PolicyEnforcer, ResourceType};
use policy_engine_sdk::gts::permissions::{Capability, RESOURCE_TYPE};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

/// Properties the PEP can compile from constraints for management content.
const PEP_PROPERTIES: &[&str] = &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// A successful authorisation.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Authorization {
    /// The scope the operation's statements run under.
    pub scope: AccessScope,
}

/// Why an authorisation did not succeed. The caller decides the projection
/// (absent for a targeted read, capability denied otherwise).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthzError {
    /// The platform does not grant the capability.
    #[error("capability not granted")]
    Denied,
    /// A dependency needed to decide could not answer.
    #[error("authorisation could not be decided: {0}")]
    Unavailable(String),
}

/// What an authorisation is about.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessTarget {
    /// Owning tenant of the content (the `owner_tenant_id` resource
    /// property); `None` for a collection operation.
    pub owner_tenant_id: Option<Uuid>,
    /// Identity of the resource, where one is targeted.
    pub resource_id: Option<Uuid>,
    /// A read of a prefetched row: the PDP may answer without row-level
    /// constraints (unconstrained scope), as in the platform's prefetch
    /// pattern.
    pub prefetched: bool,
}

impl AccessTarget {
    /// A collection operation (listing).
    #[must_use]
    pub const fn collection() -> Self {
        Self {
            owner_tenant_id: None,
            resource_id: None,
            prefetched: false,
        }
    }

    /// Content owned by `owner`.
    #[must_use]
    pub const fn owned_by(owner: Uuid) -> Self {
        Self {
            owner_tenant_id: Some(owner),
            resource_id: None,
            prefetched: false,
        }
    }

    /// Names the targeted resource.
    #[must_use]
    pub const fn resource(mut self, id: Uuid) -> Self {
        self.resource_id = Some(id);
        self
    }

    /// Marks a read of an already prefetched row.
    #[must_use]
    pub const fn prefetched(mut self) -> Self {
        self.prefetched = true;
        self
    }
}

/// One authorisation per management capability.
#[domain_model]
#[derive(Clone)]
pub struct ManagementAuthorizer {
    enforcer: PolicyEnforcer,
}

impl std::fmt::Debug for ManagementAuthorizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementAuthorizer")
            .finish_non_exhaustive()
    }
}

impl ManagementAuthorizer {
    /// Authorises through `enforcer`.
    #[must_use]
    pub const fn new(enforcer: PolicyEnforcer) -> Self {
        Self { enforcer }
    }

    /// Content read (bundles, versions, documents, assignments).
    ///
    /// # Errors
    ///
    /// [`AuthzError`].
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        target: AccessTarget,
    ) -> Result<Authorization, AuthzError> {
        self.authorize(ctx, Capability::Read, target).await
    }

    /// Authorises `capability` on `target` through the platform enforcer.
    ///
    /// # Errors
    ///
    /// [`AuthzError::Denied`] when the platform does not grant it (a deny-all
    /// scope is a denial); [`AuthzError::Unavailable`] when a dependency cannot answer.
    pub async fn authorize(
        &self,
        ctx: &SecurityContext,
        capability: Capability,
        target: AccessTarget,
    ) -> Result<Authorization, AuthzError> {
        let resource = ResourceType::new(RESOURCE_TYPE, PEP_PROPERTIES);
        let mut request = AccessRequest::new().barrier_mode(BarrierMode::Respect);
        if let Some(owner) = target.owner_tenant_id {
            request = request.resource_property(pep_properties::OWNER_TENANT_ID, owner);
        }
        if target.prefetched {
            request = request.require_constraints(false);
        }
        let scope = self
            .enforcer
            .access_scope_with(
                ctx,
                &resource,
                capability.action(),
                target.resource_id,
                &request,
            )
            .await
            .map_err(map_enforcer_error)?;
        if scope.is_deny_all() {
            return Err(AuthzError::Denied);
        }
        Ok(Authorization { scope })
    }
}

fn map_enforcer_error(err: EnforcerError) -> AuthzError {
    match err {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => AuthzError::Denied,
        EnforcerError::EvaluationFailed(e) => AuthzError::Unavailable(e.to_string()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "authz_tests.rs"]
mod authz_tests;
