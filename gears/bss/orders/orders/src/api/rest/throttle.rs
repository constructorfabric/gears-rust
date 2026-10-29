//! The pre-engine request limiter (Foundation contract §3.7, D-185; S2-12).
//!
//! Engine-entering write requests are bounded **before** the engine, because a refusal inside
//! the engine writes the very audit row the limiter exists to prevent. Two limits:
//!
//! * **Per caller — at most 200 per 60 s**, adopted from the platform api-gateway: every
//!   caller-facing engine-entering operation binds the identity-keyed zone [`CALLER_WRITE_ZONE`]
//!   through [`ThrottlingSpec`] (`require_security_context: true`, enforced, never dry-run), and
//!   the five workflow-only operations bind [`WORKFLOW_WRITE_ZONE`] when they are delivered. The
//!   zone values (`3/s`, burst 20; `50/s`, burst 100; 429 with an automatic `Retry-After`) are
//!   gateway configuration, pinned for the E2E host in `config/e2e-orders.yaml`.
//! * **Per (caller, order) — 20 per minute**, the documented gear-local REST-edge fallback of
//!   Q-26 while `cpt-cf-bss-orders-upreq-gateway-path-param-throttle-key` is open: a
//!   keyed limiter at the Orders REST edge, keyed `(subject_id, orderId)`, consulted first in
//!   every existing-order write handler, before boundary validation, authorization, the
//!   registry and the engine. A rejection is the same canonical `resource_exhausted` 429 the
//!   gateway answers, with `Retry-After`, and writes no audit, registry or access-log row.
//!
//! Both are request limiters: every request to a bound operation counts, admitted or refused,
//! so legitimate retries count too. Neither applies to the local SDK, which is in-process
//! traffic rather than gateway traffic, nor to reads, which do not enter the engine.
//!
//! Observability (D-223), after the api-gateway throttling middleware: every edge refusal
//! counts on `orders_throttle_rejections_total{kind}` (`per_order` or `max_keys`), the tracked-key
//! count is the `orders_throttle_tracked_keys` gauge, and a full key store logs a warning at most
//! once per reclamation interval. The key (`subject_id`, `orderId`) is never a metric attribute
//! and never logged.
//!
//! The gate's per-caller limiters ([`PerCallerLimiter`], keyed by the authenticated subject) bound
//! Preview (`gate.preview_per_caller_per_minute`, F03 §4.6, D-52) and submit
//! (`gate.submit_per_caller_per_minute`) with the same keyed bucket and key bound; they refuse as
//! `per_caller` / `caller_max_keys` on the same counter and gauge their keys on
//! `orders_throttle_caller_tracked_keys{limiter}`. Unlike the per-order limiter they run inside
//! the service, so the local SDK is bounded too.
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use toolkit::api::operation_builder::ThrottlingSpec;

pub use crate::infra::throttle::*;

/// The identity-keyed gateway zone of the caller-facing engine-entering operations (D-185).
pub const CALLER_WRITE_ZONE: &str = "rl_orders_caller_write";
/// The identity-keyed gateway zone of the five workflow-only operations (D-185): one service
/// principal carries every order's workflow traffic, so it must not share the caller budget.
pub const WORKFLOW_WRITE_ZONE: &str = "rl_orders_workflow_write";

/// The `ThrottlingSpec` every caller-facing engine-entering operation binds: enforced after
/// authentication so the key is the authenticated subject, never dry-run.
#[must_use]
pub fn caller_write_throttling() -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: Some(CALLER_WRITE_ZONE.to_owned()),
        in_flight_limit_zone: None,
        require_security_context: true,
        dry_run: false,
    }
}

/// The `ThrottlingSpec` of the workflow-only operations (not yet mounted; S5-08 binds it).
#[must_use]
pub fn workflow_write_throttling() -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: Some(WORKFLOW_WRITE_ZONE.to_owned()),
        in_flight_limit_zone: None,
        require_security_context: true,
        dry_run: false,
    }
}

/// The 429 the edge limiter answers: the canonical `resource_exhausted` Problem the gateway
/// uses, scoped to the Orders order resource, with the quota violation subject `throttling`,
/// its retry hint and the `Retry-After` header. The limiter adds no `RateLimit-*` headers of its
/// own: behind the gateway the caller zone decorates this (admitted) response with its own
/// `RateLimit-Remaining`, which describes the caller budget, not the per-order one; the
/// violation description tells the two apart.
#[must_use]
pub fn rejection_response(rejection: Rejection) -> Response {
    let mut response = rejection_error(rejection).into_response();
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    if let Ok(value) = HeaderValue::from_str(&rejection.retry_after_seconds().to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal is the canonical gateway shape with the Orders order scope.
    #[tokio::test]
    async fn the_rejection_is_the_canonical_resource_exhausted_problem() {
        let response = rejection_response(Rejection::PerOrderLimit {
            retry_after_seconds: 7,
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "7");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: toolkit_canonical_errors::Problem = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            problem.problem_type,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
        );
        let violations = problem.context["violations"].as_array().unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["subject"], "throttling");
        assert_eq!(violations[0]["retry_after_seconds"], 7);
    }

    /// The gateway bindings are enforced identity-keyed zones, never dry-run or IP-keyed.
    #[test]
    fn the_gateway_bindings_are_enforced_identity_zones() {
        for (spec, zone) in [
            (caller_write_throttling(), CALLER_WRITE_ZONE),
            (workflow_write_throttling(), WORKFLOW_WRITE_ZONE),
        ] {
            assert_eq!(spec.rate_limit_zone.as_deref(), Some(zone));
            assert!(spec.in_flight_limit_zone.is_none());
            assert!(spec.require_security_context);
            assert!(!spec.dry_run);
        }
        assert_ne!(CALLER_WRITE_ZONE, WORKFLOW_WRITE_ZONE);
    }
}
