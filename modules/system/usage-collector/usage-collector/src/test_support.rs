//! Shared PDP / authz mocks for in-crate unit tests.
//!
//! Centralizes the `AuthZResolverClient` mocks used by `domain::authz_tests`,
//! `domain::service_tests`, and `api::rest::handlers_tests` so that the
//! authz behavior they encode (denial reasons, fail-closed shapes, single /
//! multi-constraint outputs) stays in one place. Authz is correctness-
//! critical, so parallel definitions risked silent drift between variants.

use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, DenyReason};
use modkit_security::access_scope::pep_properties;
use uuid::Uuid;

/// Mock PDP that returns `decision=false` with a deny reason → maps to
/// `EnforcerError::Denied` → `DomainError::PermissionDenied`.
pub struct DenyAuthZ;

#[async_trait::async_trait]
impl AuthZResolverClient for DenyAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                deny_reason: Some(DenyReason {
                    error_code: "POLICY_DENIED".to_owned(),
                    details: None,
                }),
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// Mock PDP that simulates a network/infrastructure failure
/// (`AuthZResolverError::ServiceUnavailable`). The gateway authz layer maps any
/// non-Denied PDP error to `PermissionDenied` (fail-closed; see `inst-authz-3b`),
/// so this exercises the fail-closed path.
pub struct NetworkErrorAuthZ;

#[async_trait::async_trait]
impl AuthZResolverClient for NetworkErrorAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Err(AuthZResolverError::ServiceUnavailable(
            "PDP unreachable".to_owned(),
        ))
    }
}

/// Mock PDP that simulates a timeout / no plugin available
/// (`AuthZResolverError::NoPluginAvailable`).
pub struct TimeoutAuthZ;

#[async_trait::async_trait]
impl AuthZResolverClient for TimeoutAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Err(AuthZResolverError::NoPluginAvailable)
    }
}

/// Mock PDP that hangs indefinitely — used to exercise the gateway-side
/// `authz_timeout` wrapper (`RUST-ASYNC-001`). Without that wrapper a slow or
/// hung PDP would pin the request task; with it, the timeout elapses and the
/// gateway fails closed with `PermissionDenied`.
pub struct HangingAuthZ;

#[async_trait::async_trait]
impl AuthZResolverClient for HangingAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        // Type the `pending` future at the return type so the impossibility
        // is carried by the type checker — no trailing `unreachable!()` noise.
        std::future::pending::<Result<EvaluationResponse, AuthZResolverError>>().await
    }
}

/// Mock PDP that returns an internal error (engine crash).
pub struct InternalErrorAuthZ;

#[async_trait::async_trait]
impl AuthZResolverClient for InternalErrorAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Err(AuthZResolverError::Internal(
            "policy engine crashed".to_owned(),
        ))
    }
}

/// Mock PDP that returns a single tenant constraint. Used by query-handler
/// happy-path tests where the PDP must `decision=true` AND return at least one
/// constraint so `access_scope_with(require_constraints=true)` in
/// `domain/authz.rs` produces an `Ok(AccessScope)` rather than `CompileFailed`.
pub struct SingleConstraintAuthZ {
    pub tenant_id: Uuid,
}

#[async_trait::async_trait]
impl AuthZResolverClient for SingleConstraintAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [self.tenant_id],
                    ))],
                }],
                ..EvaluationResponseContext::default()
            },
        })
    }
}

/// Mock PDP that returns two OR-ed constraint groups, used to assert the
/// gateway preserves the disjunction when forwarding the compiled `AccessScope`
/// to the plugin.
pub struct MultiConstraintAuthZ {
    pub tenant_a: Uuid,
    pub tenant_b: Uuid,
}

#[async_trait::async_trait]
impl AuthZResolverClient for MultiConstraintAuthZ {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![
                    Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [self.tenant_a],
                        ))],
                    },
                    Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [self.tenant_b],
                        ))],
                    },
                ],
                ..EvaluationResponseContext::default()
            },
        })
    }
}
