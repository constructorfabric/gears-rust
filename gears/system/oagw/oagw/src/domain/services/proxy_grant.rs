//! The `proxy:invoke` check and the grant it issues.
//!
//! The REST proxy authorizes before it reads the request body (#4981) and
//! hands the grant to the data plane, which then does not ask again. The
//! grant carries the security context it was issued for, so it cannot be
//! paired with another caller, and only [`ProxyGrant::authorize`] creates
//! one.

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use oagw_sdk::PROXY_SCHEMA;
use std::time::{Duration, Instant};

use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};

use crate::domain::error::DomainError;

/// Resource type identifying a proxied upstream target.
static PROXY_RESOURCE: ResourceType =
    ResourceType::from_static(PROXY_SCHEMA, &[pep_properties::OWNER_TENANT_ID]);

/// Action name for invoking (proxying a request to) an upstream.
const INVOKE: &str = "invoke";

/// Proof that the policy engine granted `proxy:invoke` to a caller.
#[domain_model]
#[derive(Debug)]
pub(crate) struct ProxyGrant {
    ctx: SecurityContext,
    /// How long the check took: a request's metrics include it in the
    /// total, but not the time spent reading the body after it.
    authorized_in: Duration,
}

impl ProxyGrant {
    /// Ask the policy engine whether `ctx` may invoke the proxy.
    pub(crate) async fn authorize(
        enforcer: &PolicyEnforcer,
        ctx: SecurityContext,
    ) -> Result<Self, DomainError> {
        let started = Instant::now();
        enforcer
            .access_scope_with(
                &ctx,
                &PROXY_RESOURCE,
                INVOKE,
                None,
                &AccessRequest::new()
                    .require_constraints(false)
                    .context_tenant_id(ctx.subject_tenant_id()),
            )
            .await
            .inspect_err(|e| {
                if let EnforcerError::AccessDenied(authz_resolver_sdk::AccessDenial::Pdp {
                    reason: deny_reason,
                }) = e
                {
                    tracing::info!(
                        subject_id = %ctx.subject_id(),
                        tenant_id = %ctx.subject_tenant_id(),
                        resource_type = PROXY_SCHEMA,
                        action = INVOKE,
                        policy_reason = deny_reason.as_ref().map(|r| r.error_code.as_str()),
                        "OAGW proxy call denied"
                    );
                }
            })?;
        Ok(Self {
            ctx,
            authorized_in: started.elapsed(),
        })
    }

    /// The security context the grant was issued for.
    pub(crate) fn ctx(&self) -> &SecurityContext {
        &self.ctx
    }

    /// How long the check that issued the grant took.
    pub(crate) fn authorized_in(&self) -> Duration {
        self.authorized_in
    }

    /// A grant without a policy check, for tests of code that needs one.
    #[cfg(test)]
    pub(crate) fn for_tests(ctx: SecurityContext) -> Self {
        Self {
            ctx,
            authorized_in: Duration::ZERO,
        }
    }
}
