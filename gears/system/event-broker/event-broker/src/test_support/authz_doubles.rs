//! Permissive-by-default `AuthZResolverApi` double for
//! `EventBrokerHarness` (`gears-rust#4516`, `eb-authz-enforcement`).
//! Always-allow so existing/new tests that don't care about authz keep
//! passing unmodified; tests that DO care about denial construct their own
//! narrower double instead of reusing this
//! (design.md "Exact double shape ... is an implementation-time detail").
//! Tenant scope is enforced via the same `AuthZResolverApi`/
//! `PolicyEnforcer` seam (`domain/authz.rs::tenant_authorized`), not a
//! separate `tenant-resolver-sdk` double - there is no second client type
//! to fake here.

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

/// Grants every `evaluate` call with an empty context: an unconstrained
/// allow, the "no PEP configured" stand-in.
///
/// `require_constraints(false)` does not stop a PDP from returning
/// constraints, and the in-repo plugins do return them for tenant-bearing
/// requests. [`TenantScopedAuthZ`] models that shape.
pub struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Denies `evaluate` whenever `deny_if` returns `true` for the request being
/// checked, otherwise allows - lets a single test deny exactly the specific
/// `event_type:produce`/`topic:consume`/`event_type:consume`/tenant-scope
/// check it cares about (inspect `request.action.name` and
/// `request.resource.properties`) while every other check on the same
/// harness keeps its default allow.
pub struct DenyingAuthZ<F> {
    pub deny_if: F,
}

#[async_trait]
impl<F> AuthZResolverApi for DenyingAuthZ<F>
where
    F: Fn(&EvaluationRequest) -> bool + Send + Sync,
{
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: !(self.deny_if)(&request),
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Allows every `evaluate` call, scoping each tenant-bearing one to `tenants`.
///
/// A request that carries `owner_tenant_id` is answered `decision: true` with
/// one `In(owner_tenant_id, tenants)` constraint, the shape a PDP that scopes by
/// constraint returns when no resource id is given, whatever tenant was
/// requested. Other requests (named groups, `define`) get an unconstrained
/// allow, since their resources declare no tenant property to constrain.
///
/// The broker must deny a target outside `tenants`; treating the allow as
/// unconditional would let a grant in one tenant reach another.
pub struct TenantScopedAuthZ {
    pub tenants: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverApi for TenantScopedAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let constraints = if request
            .resource
            .properties
            .contains_key(pep_properties::OWNER_TENANT_ID)
        {
            vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    self.tenants.iter().copied(),
                ))],
            }]
        } else {
            Vec::new()
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..EvaluationResponseContext::default()
            },
        })
    }
}
