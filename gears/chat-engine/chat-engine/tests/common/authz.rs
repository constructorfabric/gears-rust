//! Minimal allow-with-owner-constraints PDP mock for integration tests.
//!
//! Integration binaries cannot see the crate-internal `#[cfg(test)]`
//! `test_support` harness, so this mirrors its `MockAuthZResolver` +
//! `enforcer_allow()` over the public `authz-resolver-sdk` surface: every
//! decision is a positive allow carrying owner constraints (`owner_tenant_id`
//! == subject tenant AND `owner_id` == subject id), so scoped reads/writes
//! filter to the caller's own rows.
//
// @cpt-cf-chat-engine-design-auth-model

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::pep::PolicyEnforcer;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use toolkit_security::pep_properties;
use uuid::Uuid;

fn resolve_subject_tenant(request: &EvaluationRequest) -> Option<Uuid> {
    request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tc| tc.root_id)
        .or_else(|| {
            request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .filter(|id| !id.is_nil())
}

struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverClient for MockAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let subject_id = request.subject.id;
        let constraints = match resolve_subject_tenant(&request) {
            Some(tenant) => vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [tenant])),
                    Predicate::In(InPredicate::new(pep_properties::OWNER_ID, [subject_id])),
                ],
            }],
            None => vec![],
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

/// Allow-with-owner-constraints enforcer (integration happy path).
#[must_use]
pub fn enforcer_allow() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(MockAuthZResolver))
}

/// PDP that denies every request — models a real policy rejecting a subject
/// (e.g. a cross-tenant caller). Every decision fails closed to `Forbidden`.
struct DenyAllAuthZResolver;

#[async_trait]
impl AuthZResolverClient for DenyAllAuthZResolver {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Deny-all enforcer — every decision is `Forbidden` (403).
#[must_use]
pub fn enforcer_deny() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyAllAuthZResolver))
}
