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
use std::sync::Arc;
use std::time::{Duration, Instant};

use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};

use crate::domain::error::DomainError;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{self, phase};

/// Resource type identifying a proxied upstream target.
static PROXY_RESOURCE: ResourceType =
    ResourceType::from_static(PROXY_SCHEMA, &[pep_properties::OWNER_TENANT_ID]);

/// Action name for invoking (proxying a request to) an upstream.
const INVOKE: &str = "invoke";

/// Proof that the policy engine granted `proxy:invoke` to a caller.
///
/// A grant issued for a request that the metrics count (see
/// [`Self::count_if_dropped`]) counts it as `Cancelled` when it is dropped
/// before [`Self::take_count`] hands the counting over: the client went
/// away, or the caller was dropped, before the request was proxied or
/// rejected.
#[domain_model]
pub(crate) struct ProxyGrant {
    ctx: SecurityContext,
    /// How long the check took: a request's metrics include it in the
    /// total, but not the time spent reading the body after it.
    authorized_in: Duration,
    /// Who counts the request while nobody else does.
    count: Option<GrantCount>,
}

/// The request metrics a grant holds until it is proxied or rejected.
struct GrantCount {
    metrics: Arc<dyn OagwMetricsPort>,
    /// The request's `method` metric label.
    method: &'static str,
}

impl std::fmt::Debug for ProxyGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyGrant")
            .field("ctx", &self.ctx)
            .field("authorized_in", &self.authorized_in)
            .field("counted", &self.count.is_some())
            .finish()
    }
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
            count: None,
        })
    }

    /// Count the request in `metrics`, under the `method` label, as
    /// `Cancelled` if the grant is dropped before [`Self::take_count`].
    pub(crate) fn count_if_dropped(
        mut self,
        metrics: Arc<dyn OagwMetricsPort>,
        method: &'static str,
    ) -> Self {
        self.count = Some(GrantCount { metrics, method });
        self
    }

    /// Hand the counting of the request over to the caller: the metrics and
    /// the `method` label of [`Self::count_if_dropped`], if it was called.
    /// The grant then drops without counting anything.
    pub(crate) fn take_count(&mut self) -> Option<(Arc<dyn OagwMetricsPort>, &'static str)> {
        self.count.take().map(|c| (c.metrics, c.method))
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
            count: None,
        }
    }
}

impl Drop for ProxyGrant {
    fn drop(&mut self) {
        let Some(GrantCount { metrics, .. }) = self.count.take() else {
            return;
        };
        // Timed as a proxied request is: the check, not the client's upload.
        // No status: the client got no response.
        let unknown = metric_labels::UNKNOWN;
        metrics.record_request_duration_seconds(
            unknown,
            unknown,
            phase::TOTAL,
            self.authorized_in.as_secs_f64(),
        );
        metrics.record_error(unknown, unknown, metric_labels::CANCELLED, None);
    }
}
