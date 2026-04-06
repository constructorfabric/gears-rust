use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode};
use modkit::api::problem::Problem;

use crate::domain::error::DomainError;
use oagw_sdk::api::ErrorSource;

// ---------------------------------------------------------------------------
// GTS error type constants
// ---------------------------------------------------------------------------

pub(crate) const ERR_VALIDATION: &str = "gts.x.core.errors.err.v1~x.oagw.validation.error.v1";
pub(crate) const ERR_CONFLICT: &str = "gts.x.core.errors.err.v1~x.oagw.resource.conflict.v1";
pub(crate) const ERR_MISSING_TARGET_HOST: &str =
    "gts.x.core.errors.err.v1~x.oagw.routing.missing_target_host.v1";
pub(crate) const ERR_INVALID_TARGET_HOST: &str =
    "gts.x.core.errors.err.v1~x.oagw.routing.invalid_target_host.v1";
pub(crate) const ERR_UNKNOWN_TARGET_HOST: &str =
    "gts.x.core.errors.err.v1~x.oagw.routing.unknown_target_host.v1";
pub(crate) const ERR_AUTH_FAILED: &str = "gts.x.core.errors.err.v1~x.oagw.auth.failed.v1";
pub(crate) const ERR_NOT_FOUND: &str = "gts.x.core.errors.err.v1~x.oagw.resource.not_found.v1";
pub(crate) const ERR_ROUTE_NOT_FOUND: &str = "gts.x.core.errors.err.v1~x.oagw.route.not_found.v1";
pub(crate) const ERR_PAYLOAD_TOO_LARGE: &str =
    "gts.x.core.errors.err.v1~x.oagw.payload.too_large.v1";
pub(crate) const ERR_RATE_LIMIT_EXCEEDED: &str =
    "gts.x.core.errors.err.v1~x.oagw.rate_limit.exceeded.v1";
pub(crate) const ERR_SECRET_NOT_FOUND: &str = "gts.x.core.errors.err.v1~x.oagw.secret.not_found.v1";
pub(crate) const ERR_DOWNSTREAM: &str = "gts.x.core.errors.err.v1~x.oagw.downstream.error.v1";
pub(crate) const ERR_PROTOCOL: &str = "gts.x.core.errors.err.v1~x.oagw.protocol.error.v1";
pub(crate) const ERR_UPSTREAM_DISABLED: &str =
    "gts.x.core.errors.err.v1~x.oagw.routing.upstream_disabled.v1";
pub(crate) const ERR_CONNECTION_TIMEOUT: &str =
    "gts.x.core.errors.err.v1~x.oagw.timeout.connection.v1";
pub(crate) const ERR_REQUEST_TIMEOUT: &str = "gts.x.core.errors.err.v1~x.oagw.timeout.request.v1";
pub(crate) const ERR_GUARD_REJECTED: &str = "gts.x.core.errors.err.v1~x.oagw.guard.rejected.v1";
pub(crate) const ERR_CORS_ORIGIN_NOT_ALLOWED: &str =
    "gts.x.core.errors.err.v1~x.oagw.cors.origin_not_allowed.v1";
pub(crate) const ERR_CORS_METHOD_NOT_ALLOWED: &str =
    "gts.x.core.errors.err.v1~x.oagw.cors.method_not_allowed.v1";
pub(crate) const ERR_STREAM_ABORTED: &str = "gts.x.core.errors.err.v1~x.oagw.stream.aborted.v1";
pub(crate) const ERR_LINK_UNAVAILABLE: &str = "gts.x.core.errors.err.v1~x.oagw.link.unavailable.v1";
pub(crate) const ERR_CIRCUIT_BREAKER_OPEN: &str =
    "gts.x.core.errors.err.v1~x.oagw.circuit_breaker.open.v1";
pub(crate) const ERR_IDLE_TIMEOUT: &str = "gts.x.core.errors.err.v1~x.oagw.timeout.idle.v1";
pub(crate) const ERR_PLUGIN_NOT_FOUND: &str = "gts.x.core.errors.err.v1~x.oagw.plugin.not_found.v1";
pub(crate) const ERR_PLUGIN_IN_USE: &str = "gts.x.core.errors.err.v1~x.oagw.plugin.in_use.v1";
pub(crate) const ERR_FORBIDDEN: &str = "gts.x.core.errors.err.v1~x.oagw.authz.forbidden.v1";

// ---------------------------------------------------------------------------
// DomainError → Problem helpers
// ---------------------------------------------------------------------------

fn gts_type(err: &DomainError) -> &str {
    match err {
        DomainError::Validation { .. } => ERR_VALIDATION,
        DomainError::Conflict { .. } => ERR_CONFLICT,
        DomainError::MissingTargetHost { .. } => ERR_MISSING_TARGET_HOST,
        DomainError::InvalidTargetHost { .. } => ERR_INVALID_TARGET_HOST,
        DomainError::UnknownTargetHost { .. } => ERR_UNKNOWN_TARGET_HOST,
        DomainError::AuthenticationFailed { .. } => ERR_AUTH_FAILED,
        DomainError::NotFound {
            entity: "route", ..
        } => ERR_ROUTE_NOT_FOUND,
        DomainError::NotFound { .. } => ERR_NOT_FOUND,
        DomainError::PayloadTooLarge { .. } => ERR_PAYLOAD_TOO_LARGE,
        DomainError::RateLimitExceeded { .. } => ERR_RATE_LIMIT_EXCEEDED,
        DomainError::SecretNotFound { .. } => ERR_SECRET_NOT_FOUND,
        DomainError::DownstreamError { .. } | DomainError::Internal { .. } => ERR_DOWNSTREAM,
        DomainError::ProtocolError { .. } => ERR_PROTOCOL,
        DomainError::UpstreamDisabled { .. } => ERR_UPSTREAM_DISABLED,
        DomainError::ConnectionTimeout { .. } => ERR_CONNECTION_TIMEOUT,
        DomainError::RequestTimeout { .. } => ERR_REQUEST_TIMEOUT,
        DomainError::GuardRejected { .. } => ERR_GUARD_REJECTED,
        DomainError::CorsOriginNotAllowed { .. } => ERR_CORS_ORIGIN_NOT_ALLOWED,
        DomainError::CorsMethodNotAllowed { .. } => ERR_CORS_METHOD_NOT_ALLOWED,
        DomainError::StreamAborted { .. } => ERR_STREAM_ABORTED,
        DomainError::LinkUnavailable { .. } => ERR_LINK_UNAVAILABLE,
        DomainError::CircuitBreakerOpen { .. } => ERR_CIRCUIT_BREAKER_OPEN,
        DomainError::IdleTimeout { .. } => ERR_IDLE_TIMEOUT,
        DomainError::PluginNotFound { .. } => ERR_PLUGIN_NOT_FOUND,
        DomainError::PluginInUse { .. } => ERR_PLUGIN_IN_USE,
        DomainError::Forbidden { .. } => ERR_FORBIDDEN,
    }
}

fn http_status_code(err: &DomainError) -> StatusCode {
    match err {
        DomainError::Validation { .. }
        | DomainError::MissingTargetHost { .. }
        | DomainError::InvalidTargetHost { .. }
        | DomainError::UnknownTargetHost { .. } => StatusCode::BAD_REQUEST,
        DomainError::Conflict { .. } => StatusCode::CONFLICT,
        DomainError::AuthenticationFailed { .. } => StatusCode::UNAUTHORIZED,
        DomainError::NotFound { .. } => StatusCode::NOT_FOUND,
        DomainError::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        DomainError::RateLimitExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
        DomainError::SecretNotFound { .. } | DomainError::Internal { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        DomainError::DownstreamError { .. } | DomainError::ProtocolError { .. } => {
            StatusCode::BAD_GATEWAY
        }
        DomainError::UpstreamDisabled { .. }
        | DomainError::LinkUnavailable { .. }
        | DomainError::CircuitBreakerOpen { .. } => StatusCode::SERVICE_UNAVAILABLE,
        DomainError::ConnectionTimeout { .. }
        | DomainError::RequestTimeout { .. }
        | DomainError::IdleTimeout { .. } => StatusCode::GATEWAY_TIMEOUT,
        DomainError::StreamAborted { .. } => StatusCode::BAD_GATEWAY,
        DomainError::PluginNotFound { .. } => StatusCode::NOT_FOUND,
        DomainError::PluginInUse { .. } => StatusCode::CONFLICT,
        DomainError::GuardRejected { status, .. } => StatusCode::from_u16(*status)
            .ok()
            .filter(|code| code.is_client_error() || code.is_server_error())
            .unwrap_or(StatusCode::BAD_REQUEST),
        DomainError::CorsOriginNotAllowed { .. } | DomainError::CorsMethodNotAllowed { .. } => {
            StatusCode::FORBIDDEN
        }
        DomainError::Forbidden { .. } => StatusCode::FORBIDDEN,
    }
}

fn error_title(err: &DomainError) -> &str {
    match err {
        DomainError::Validation { .. } => "Validation Error",
        DomainError::Conflict { .. } => "Conflict",
        DomainError::MissingTargetHost { .. } => "Missing Target Host",
        DomainError::InvalidTargetHost { .. } => "Invalid Target Host",
        DomainError::UnknownTargetHost { .. } => "Unknown Target Host",
        DomainError::AuthenticationFailed { .. } => "Authentication Failed",
        DomainError::NotFound { .. } => "Not Found",
        DomainError::PayloadTooLarge { .. } => "Payload Too Large",
        DomainError::RateLimitExceeded { .. } => "Rate Limit Exceeded",
        DomainError::SecretNotFound { .. } => "Secret Not Found",
        DomainError::DownstreamError { .. } | DomainError::Internal { .. } => "Downstream Error",
        DomainError::ProtocolError { .. } => "Protocol Error",
        DomainError::UpstreamDisabled { .. } => "Upstream Disabled",
        DomainError::ConnectionTimeout { .. } => "Connection Timeout",
        DomainError::RequestTimeout { .. } => "Request Timeout",
        DomainError::GuardRejected { .. } => "Guard Rejected",
        DomainError::CorsOriginNotAllowed { .. } => "CORS Origin Not Allowed",
        DomainError::CorsMethodNotAllowed { .. } => "CORS Method Not Allowed",
        DomainError::StreamAborted { .. } => "Stream Aborted",
        DomainError::LinkUnavailable { .. } => "Link Unavailable",
        DomainError::CircuitBreakerOpen { .. } => "Circuit Breaker Open",
        DomainError::IdleTimeout { .. } => "Idle Timeout",
        DomainError::PluginNotFound { .. } => "Plugin Not Found",
        DomainError::PluginInUse { .. } => "Plugin In Use",
        DomainError::Forbidden { .. } => "Forbidden",
    }
}

fn error_instance(err: &DomainError) -> &str {
    match err {
        DomainError::Validation { instance, .. }
        | DomainError::MissingTargetHost { instance, .. }
        | DomainError::InvalidTargetHost { instance, .. }
        | DomainError::UnknownTargetHost { instance, .. }
        | DomainError::AuthenticationFailed { instance, .. }
        | DomainError::PayloadTooLarge { instance, .. }
        | DomainError::RateLimitExceeded { instance, .. }
        | DomainError::SecretNotFound { instance, .. }
        | DomainError::DownstreamError { instance, .. }
        | DomainError::ProtocolError { instance, .. }
        | DomainError::ConnectionTimeout { instance, .. }
        | DomainError::RequestTimeout { instance, .. }
        | DomainError::GuardRejected { instance, .. }
        | DomainError::CorsOriginNotAllowed { instance, .. }
        | DomainError::CorsMethodNotAllowed { instance, .. }
        | DomainError::StreamAborted { instance, .. }
        | DomainError::LinkUnavailable { instance, .. }
        | DomainError::CircuitBreakerOpen { instance, .. }
        | DomainError::IdleTimeout { instance, .. } => instance,
        DomainError::NotFound { .. }
        | DomainError::Conflict { .. }
        | DomainError::UpstreamDisabled { .. }
        | DomainError::Internal { .. }
        | DomainError::PluginNotFound { .. }
        | DomainError::PluginInUse { .. }
        | DomainError::Forbidden { .. } => "",
    }
}

// ---------------------------------------------------------------------------
// From<DomainError> for Problem
// ---------------------------------------------------------------------------

impl From<DomainError> for Problem {
    fn from(err: DomainError) -> Self {
        let gts = gts_type(&err).to_string();
        let inst = error_instance(&err).to_string();
        let status = http_status_code(&err);
        let t = error_title(&err).to_string();
        let detail = err.to_string();

        Problem::new(status, t, detail)
            .with_type(gts)
            .with_instance(inst)
    }
}

// ---------------------------------------------------------------------------
// Convenience functions for handlers
// ---------------------------------------------------------------------------

/// Convert a `DomainError` into a `Problem`, filling in `instance` for
/// variants that don't carry their own. Used by management API handlers.
pub(crate) fn domain_error_to_problem(err: DomainError, instance: &str) -> Problem {
    let mut p = Problem::from(err);
    if p.instance.is_empty() {
        p.instance = instance.to_string();
    }
    p
}

/// Convert a `DomainError` into an axum `Response` with the
/// `x-oagw-error-source: gateway` header. Used by the proxy handler.
pub fn error_response(err: DomainError) -> Response {
    let retry_after = match &err {
        DomainError::RateLimitExceeded {
            retry_after_secs: Some(secs),
            ..
        } => Some(*secs),
        _ => None,
    };

    let problem: Problem = err.into();
    let mut response = problem.into_response();

    response.headers_mut().insert(
        "x-oagw-error-source",
        HeaderValue::from_static(ErrorSource::Gateway.as_str()),
    );

    if let Some(secs) = retry_after
        && let Ok(v) = secs.to_string().parse()
    {
        response.headers_mut().insert("retry-after", v);
    }

    response
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
