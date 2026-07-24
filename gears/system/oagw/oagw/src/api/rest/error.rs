//! REST error mapping for the OAGW gear.
//!
//! Maps `DomainError` to canonical errors (`toolkit-canonical-errors`) and
//! provides:
//!
//! * `From<DomainError> for CanonicalError` — long-lived mapping.
//! * `domain_error_to_problem` — convenience for management handlers that
//!   already plumb the request URI through. Pre-populates `Problem.instance`
//!   from the supplied request URI so the canonical error middleware does
//!   not overwrite it (the middleware fills `instance` only when it is
//!   empty).
//! * `error_response` — proxy-pipeline helper that produces an axum
//!   `Response` with the gateway-specific `x-oagw-error-source`,
//!   `Retry-After`, and `x-ratelimit-*` headers. `instance` / `trace_id`
//!   are filled by the canonical error middleware on the way out.
//!
//! ## `permission_denied` is fixed-detail by design
//!
//! `toolkit-canonical-errors-macro` generates a zero-argument
//! `permission_denied()` constructor — the wire `detail` is locked to the
//! canonical default (`"You do not have permission to perform this
//! operation"`) regardless of cause (CORS origin, CORS method, RBAC
//! denial, guard plugin 403, …). This is a deliberate macro contract, not
//! an oversight. Do not try to splice variable data into the `reason`
//! field to compensate (earlier revisions of this file did, and the wire
//! `reason` ended up carrying user-supplied origins/methods/principal
//! ids).
//!
//! The differentiator on the wire is the structured `reason` field —
//! consumers branch on `reason` (`CORS_ORIGIN_NOT_ALLOWED`,
//! `CORS_METHOD_NOT_ALLOWED`, `AUTHZ_DENIED`, plugin-supplied codes),
//! and variable diagnostic data (the actual origin / method / principal /
//! plugin detail) lives in server-side `tracing::debug!` events
//! correlated by `trace_id`.
//!
//! Future work: if it becomes necessary to surface variable detail on the
//! wire for a 403, do it via a derived GTS type and the reserved `extra`
//! field (DESIGN.md §3.8) rather than fighting the macro contract.

use axum::response::{IntoResponse, Response};
use http::HeaderValue;
use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

use crate::domain::error::DomainError;
use crate::domain::gts_helpers as gts;
use oagw_sdk::api::ErrorSource;
use oagw_sdk::{field, quota, reason};

// ---------------------------------------------------------------------------
// Retry-after defaults for `service_unavailable` emissions
//
// `categories/14-service-unavailable.md` requires `retry_after_seconds`
// in the wire context. The values below are coarse heuristics chosen per
// failure mode — short for transient upstream issues, longer for
// configuration / circuit-breaker windows where retrying immediately is
// wasted work.
// ---------------------------------------------------------------------------

/// Transient upstream / protocol / stream errors — likely to clear quickly.
const RETRY_AFTER_TRANSIENT_SECS: u64 = 5;

/// No-route / connect-failure errors — retry once DNS/network settles.
const RETRY_AFTER_LINK_SECS: u64 = 10;

/// Circuit-breaker-open and admin-disabled upstreams — bounded by the
/// circuit-breaker recovery window or operator action.
const RETRY_AFTER_CIRCUIT_BREAKER_SECS: u64 = 30;

/// Upstream-disabled by configuration — operator action required.
const RETRY_AFTER_ADMIN_SECS: u64 = 30;

/// Unknown 5xx from a guard plugin — conservative default.
const RETRY_AFTER_GUARD_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// Resource error scopes
//
// All four scopes use the GTS identifiers oagw already exposes via
// `domain::gts_helpers` — clients that recognise the existing
// upstream/route/proxy/guard_plugin types continue to interpret the
// `resource_type` field unchanged.
// ---------------------------------------------------------------------------

/// Errors attributable to the gateway data-plane / proxy as a resource.
/// Used as the umbrella scope when no more specific resource applies.
#[resource_error(gts_id!("cf.core.oagw.proxy.v1~"))]
pub struct OagwProxyError;

/// Errors attributable to a specific upstream definition.
#[resource_error(gts_id!("cf.core.oagw.upstream.v1~"))]
pub struct OagwUpstreamError;

/// Errors attributable to a specific route definition.
#[resource_error(gts_id!("cf.core.oagw.route.v1~"))]
pub struct OagwRouteError;

/// Errors attributable to a specific authentication plugin instance.
#[resource_error(gts_id!("cf.core.oagw.auth_plugin.v1~"))]
pub struct OagwAuthPluginError;

/// Errors raised by guard plugins during the proxy pipeline.
#[resource_error(gts_id!("cf.core.oagw.guard_plugin.v1~"))]
pub struct OagwGuardPluginError;

/// Errors attributable to a specific transform plugin instance.
#[resource_error(gts_id!("cf.core.oagw.transform_plugin.v1~"))]
pub struct OagwTransformPluginError;

// ---------------------------------------------------------------------------
// DomainError → CanonicalError
// ---------------------------------------------------------------------------

// Per-arm `tracing::*` calls below are deliberate, not transitional. The
// canonical error middleware (`toolkit::api::canonical_error_middleware`)
// logs status/problem_type/instance/trace_id for every Problem response,
// plus the `Internal::description` / `Unknown::description` diagnostic
// recovered from the `CanonicalError` axum extension (DESIGN.md §3.6).
// The remaining per-arm calls preserve variable diagnostic data that the
// canonical envelope cannot carry:
//   * `service_unavailable` — wire `detail` collapses to a generic string,
//     so the variable `detail`/`upstream` fields live only in tracing.
//   * `permission_denied` — the macro contract locks the wire `detail`
//     and the `reason` field cannot carry user-supplied
//     origins/methods/principal ids (see gear docs above).
//   * `unauthenticated` — the variable `detail` is not placed on the wire
//     (wire context carries only the `reason` discriminator).
impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                if field.is_empty() {
                    // No specific field → use the canonical Format variant
                    // so the wire response stays accurate (the detail
                    // surfaces directly without lying about which field
                    // failed).
                    OagwProxyError::invalid_argument()
                        .with_format(detail)
                        .create()
                } else {
                    OagwProxyError::invalid_argument()
                        .with_field_violation(field, detail, reason)
                        .create()
                }
            }

            // Dispatch on the entity discriminator so the wire
            // `resource_type` matches the resource the conflict is about
            // (alias collision → upstream; route-table overlap → route).
            DomainError::Conflict {
                entity,
                resource,
                detail,
            } => match entity {
                "upstream" => OagwUpstreamError::already_exists(detail)
                    .with_resource(resource)
                    .create(),
                "route" => OagwRouteError::already_exists(detail)
                    .with_resource(resource)
                    .create(),
                _ => OagwProxyError::already_exists(detail)
                    .with_resource(resource)
                    .create(),
            },

            DomainError::MissingTargetHost { .. } => OagwProxyError::invalid_argument()
                .with_field_violation(
                    "x-target-host",
                    "target host header required for multi-endpoint upstream",
                    field::MISSING_TARGET_HOST,
                )
                .create(),

            DomainError::InvalidTargetHost { .. } => OagwProxyError::invalid_argument()
                .with_field_violation(
                    "x-target-host",
                    "invalid target host header format",
                    field::INVALID_TARGET_HOST,
                )
                .create(),

            DomainError::UnknownTargetHost { detail, .. } => OagwProxyError::invalid_argument()
                .with_field_violation("x-target-host", detail, field::UNKNOWN_TARGET_HOST)
                .create(),

            DomainError::AuthenticationFailed { reason, detail, .. } => {
                tracing::debug!(reason, detail = %detail, "OAGW authentication failed");
                CanonicalError::unauthenticated()
                    .with_reason(reason)
                    .create()
            }

            // The `entity` discriminator already encodes which resource
            // type the missing id belongs to; dispatch on it so the wire
            // `resource_type` matches the entity oagw exposes.
            DomainError::NotFound { entity, id } => match entity {
                "upstream" => OagwUpstreamError::not_found(format!("upstream not found: {id}"))
                    .with_resource(id.to_string())
                    .create(),
                "route" => OagwRouteError::not_found(format!("route not found: {id}"))
                    .with_resource(id.to_string())
                    .create(),
                _ => OagwProxyError::not_found(format!("{entity} not found: {id}"))
                    .with_resource(id.to_string())
                    .create(),
            },

            DomainError::PayloadTooLarge { detail, .. } => {
                OagwProxyError::out_of_range(detail.clone())
                    .with_field_violation("body", detail, field::PAYLOAD_TOO_LARGE)
                    .create()
            }

            DomainError::RateLimitExceeded { detail, .. } => {
                OagwProxyError::resource_exhausted(detail.clone())
                    .with_quota_violation(quota::RATE_LIMIT, detail)
                    .create()
            }

            DomainError::SecretNotFound { detail, .. } => {
                // `CanonicalError::internal(detail)` captures the detail in
                // `Internal::description`; the canonical error middleware
                // surfaces it server-side via the `CanonicalError` extension
                // (DESIGN.md §3.6), so no per-arm log is needed.
                CanonicalError::internal(detail).create()
            }

            // A state precondition, not a malformed request: the referenced
            // secret is not provisioned yet or not shared. Kept distinct from
            // `Validation`/`invalid_argument` so in-process provisioning
            // callers can classify it as retryable (HTTP status is 400 for
            // both, but the canonical category differs).
            DomainError::SecretRefNotAccessible { detail, .. } => {
                OagwUpstreamError::failed_precondition()
                    .with_precondition_violation("auth.config.secret_ref", detail, "STATE")
                    .create()
            }

            DomainError::DownstreamError { detail, .. } => {
                tracing::warn!(reason = %detail, "OAGW downstream error");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
                    .create()
            }

            DomainError::ProtocolError { detail, .. } => {
                tracing::warn!(reason = %detail, "OAGW upstream protocol error");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
                    .create()
            }

            DomainError::UpstreamDisabled { alias } => {
                tracing::debug!(upstream = %alias, "OAGW upstream is disabled");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_ADMIN_SECS)
                    .create()
            }

            DomainError::ConnectionTimeout { detail, .. } => {
                OagwProxyError::deadline_exceeded(detail).create()
            }

            DomainError::RequestTimeout { detail, .. } => {
                OagwProxyError::deadline_exceeded(detail).create()
            }

            DomainError::Internal { message } => {
                // See SecretNotFound: the canonical error middleware logs
                // `Internal::description` via the extension path, so the
                // per-arm `tracing::error!` would be redundant.
                CanonicalError::internal(message).create()
            }

            DomainError::GuardRejected {
                status,
                error_code,
                detail,
                resource_id,
                ..
            } => guard_rejected_to_canonical(status, error_code, detail, resource_id),

            DomainError::CorsOriginNotAllowed { origin, .. } => {
                tracing::debug!(origin = %origin, "OAGW CORS origin rejected");
                OagwProxyError::permission_denied()
                    .with_reason(reason::permission::CORS_ORIGIN_NOT_ALLOWED)
                    .create()
            }

            DomainError::CorsMethodNotAllowed { method, .. } => {
                tracing::debug!(method = %method, "OAGW CORS method rejected");
                OagwProxyError::permission_denied()
                    .with_reason(reason::permission::CORS_METHOD_NOT_ALLOWED)
                    .create()
            }

            DomainError::StreamAborted { detail, .. } => {
                tracing::warn!(reason = %detail, "OAGW upstream stream aborted");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
                    .create()
            }

            DomainError::LinkUnavailable { detail, .. } => {
                tracing::warn!(reason = %detail, "OAGW upstream link unavailable");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_LINK_SECS)
                    .create()
            }

            DomainError::CircuitBreakerOpen { detail, .. } => {
                tracing::warn!(reason = %detail, "OAGW circuit breaker open");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_CIRCUIT_BREAKER_SECS)
                    .create()
            }

            DomainError::IdleTimeout { detail, .. } => {
                OagwProxyError::deadline_exceeded(detail).create()
            }

            // The `gts_id` prefix encodes the plugin kind, so dispatch
            // through the matching per-kind resource scope. The wire
            // `resource_name` carries the actual plugin GTS identifier.
            DomainError::PluginNotFound { gts_id, detail } => {
                plugin_not_found_to_canonical(gts_id, detail)
            }

            DomainError::PluginInUse { gts_id, detail } => {
                plugin_in_use_to_canonical(gts_id, detail)
            }

            DomainError::Forbidden { reason, detail } => {
                tracing::debug!(reason = %reason, detail = %detail, "OAGW authorization denied");
                OagwProxyError::permission_denied()
                    .with_reason(reason)
                    .create()
            }
        }
    }
}

/// Route a plugin-not-found error to the resource scope matching the
/// plugin kind encoded in the GTS identifier prefix. Unknown prefixes
/// fall back to the proxy umbrella so the canonical category is still
/// honoured.
fn plugin_not_found_to_canonical(gts_id: String, detail: String) -> CanonicalError {
    if gts_id.starts_with(gts::AUTH_PLUGIN_SCHEMA) {
        OagwAuthPluginError::not_found(detail)
            .with_resource(gts_id)
            .create()
    } else if gts_id.starts_with(gts::GUARD_PLUGIN_SCHEMA) {
        OagwGuardPluginError::not_found(detail)
            .with_resource(gts_id)
            .create()
    } else if gts_id.starts_with(gts::TRANSFORM_PLUGIN_SCHEMA) {
        OagwTransformPluginError::not_found(detail)
            .with_resource(gts_id)
            .create()
    } else {
        OagwProxyError::not_found(detail)
            .with_resource(gts_id)
            .create()
    }
}

/// Companion to [`plugin_not_found_to_canonical`] for the in-use case.
fn plugin_in_use_to_canonical(gts_id: String, detail: String) -> CanonicalError {
    if gts_id.starts_with(gts::AUTH_PLUGIN_SCHEMA) {
        OagwAuthPluginError::already_exists(detail)
            .with_resource(gts_id)
            .create()
    } else if gts_id.starts_with(gts::GUARD_PLUGIN_SCHEMA) {
        OagwGuardPluginError::already_exists(detail)
            .with_resource(gts_id)
            .create()
    } else if gts_id.starts_with(gts::TRANSFORM_PLUGIN_SCHEMA) {
        OagwTransformPluginError::already_exists(detail)
            .with_resource(gts_id)
            .create()
    } else {
        OagwProxyError::already_exists(detail)
            .with_resource(gts_id)
            .create()
    }
}

/// Map a plugin-supplied HTTP status (from `DomainError::GuardRejected`) to
/// the closest canonical category. Unknown statuses fall back to
/// `invalid_argument` so the canonical taxonomy is always satisfied.
///
/// `resource_id` is the optional identifier of the resource the rejection
/// refers to. When set, 404/409 rejections route through the canonical
/// `not_found` / `already_exists` categories with the id as
/// `resource_name`. When `None`, they fall back to `failed_precondition`
/// / `aborted` so the wire body stays schema-compliant without a
/// placeholder identifier.
fn guard_rejected_to_canonical(
    status: u16,
    error_code: String,
    detail: String,
    resource_id: Option<String>,
) -> CanonicalError {
    match status {
        400 | 422 => OagwGuardPluginError::invalid_argument()
            .with_field_violation("guard", detail, error_code)
            .create(),
        401 => CanonicalError::unauthenticated()
            .with_reason(error_code)
            .create(),
        403 => {
            tracing::debug!(reason = %detail, "OAGW guard rejected with 403");
            OagwGuardPluginError::permission_denied()
                .with_reason(error_code)
                .create()
        }
        404 => match resource_id {
            Some(id) => OagwGuardPluginError::not_found(detail)
                .with_resource(id)
                .create(),
            None => OagwGuardPluginError::failed_precondition()
                .with_precondition_violation(error_code, detail, "STATE")
                .create(),
        },
        409 => match resource_id {
            Some(id) => OagwGuardPluginError::already_exists(detail)
                .with_resource(id)
                .create(),
            None => OagwGuardPluginError::aborted(detail)
                .with_reason(error_code)
                .create(),
        },
        413 => OagwGuardPluginError::out_of_range(detail.clone())
            .with_field_violation("guard", detail, error_code)
            .create(),
        429 => OagwGuardPluginError::resource_exhausted(detail.clone())
            .with_quota_violation("guard", detail)
            .create(),
        // The canonical `service_unavailable` schema (categories/14) only
        // carries `retry_after_seconds` — the upstream-supplied 5xx
        // status and `error_code` cannot be placed on the wire. Surface
        // them server-side at WARN (DESIGN §3.6) so operators can
        // correlate the masked failure via `trace_id` without exposing
        // upstream internals.
        500..=599 => {
            tracing::warn!(
                upstream_status = status,
                upstream_error_code = %error_code,
                detail = %detail,
                "OAGW guard plugin returned 5xx — masking as service_unavailable",
            );
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(RETRY_AFTER_GUARD_SECS)
                .create()
        }
        _ => OagwGuardPluginError::invalid_argument()
            .with_field_violation("guard", detail, error_code)
            .create(),
    }
}

// ---------------------------------------------------------------------------
// Convenience functions for handlers
// ---------------------------------------------------------------------------

/// Convert a `DomainError` into a canonical `Problem`, pre-populating
/// `instance` from the supplied request URI. Used by management API
/// handlers that already plumb `instance` through.
///
/// `instance` is set explicitly so the canonical error middleware does
/// not overwrite it on the way out (the middleware fills `instance` only
/// when it is empty).
pub(crate) fn domain_error_to_problem(err: DomainError, instance: &str) -> Problem {
    let mut problem = Problem::from(CanonicalError::from(err));
    problem.instance = Some(instance.to_owned());
    problem
}

/// Convert a `DomainError` into an axum `Response` with the
/// `x-oagw-error-source: gateway` header. Used by the proxy data-plane.
///
/// Rate-limit metadata (when present in `DomainError::RateLimitExceeded`)
/// is converted into wire headers (`Retry-After`, `X-RateLimit-*`) and the
/// detail is sanitised to avoid leaking internal key structure (resource
/// IDs, tenant IDs, scope) in the body.
///
/// `instance` / `trace_id` are NOT set here — the canonical error
/// middleware (`toolkit::api::canonical_error_middleware`) injects them on
/// the response on the way out via the same router this helper feeds into.
pub fn error_response(err: DomainError) -> Response {
    let rate_limit_meta = match &err {
        DomainError::RateLimitExceeded {
            retry_after_secs,
            limit,
            remaining,
            reset_epoch,
            ..
        } => Some((*retry_after_secs, *limit, *remaining, *reset_epoch)),
        _ => None,
    };

    let canonical = CanonicalError::from(err);
    let mut problem = Problem::from(canonical);

    if rate_limit_meta.is_some() {
        problem.detail =
            "Rate limit exceeded. Retry after the duration indicated by the Retry-After header."
                .to_string();
    }

    let mut response = problem.into_response();

    response.headers_mut().insert(
        "x-oagw-error-source",
        HeaderValue::from_static(ErrorSource::Gateway.as_str()),
    );

    if let Some((retry_after_secs, limit, remaining, reset_epoch)) = rate_limit_meta {
        if let Some(secs) = retry_after_secs
            && let Ok(v) = secs.to_string().parse()
        {
            response.headers_mut().insert("retry-after", v);
        }
        if let Some(l) = limit
            && let Ok(v) = l.to_string().parse()
        {
            response.headers_mut().insert("x-ratelimit-limit", v);
        }
        if let Some(r) = remaining
            && let Ok(v) = r.to_string().parse()
        {
            response.headers_mut().insert("x-ratelimit-remaining", v);
        }
        if let Some(re) = reset_epoch
            && let Ok(v) = re.to_string().parse()
        {
            response.headers_mut().insert("x-ratelimit-reset", v);
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_gts::{gts_id, gts_uri};

    /// Build the wire `Problem` the canonical error middleware would emit
    /// for a given `DomainError`. Tests run without the middleware in
    /// scope, so `instance` / `trace_id` stay `None` here — that injection
    /// is exercised by integration tests that drive the full router.
    trait IntoTestProblem {
        fn into_test_problem(self) -> Problem;
    }

    impl<E> IntoTestProblem for E
    where
        CanonicalError: From<E>,
    {
        fn into_test_problem(self) -> Problem {
            Problem::from(CanonicalError::from(self))
        }
    }

    const NOT_FOUND_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.not_found.v1~");
    const INVALID_ARGUMENT_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~");
    const ALREADY_EXISTS_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.already_exists.v1~");
    const RESOURCE_EXHAUSTED_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~");

    #[test]
    fn validation_with_field_emits_field_violation() {
        let err = DomainError::Validation {
            field: "server",
            reason: field::REQUIRED,
            detail: "missing required field 'server'".into(),
            instance: "/oagw/v1/upstreams".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        assert_eq!(p.title, "Invalid Argument");
        // `instance` is filled by the canonical error middleware on the
        // way out; at the conversion layer it stays `None`. The end-to-end
        // wire value is exercised by integration tests that drive the
        // full router.
        assert!(p.instance.is_none());
        // Field-scoped variant carries the violation in
        // `context.field_violations[]` per the canonical FieldViolations
        // schema; the top-level detail stays the canonical default.
        let violations = p
            .context
            .get("field_violations")
            .and_then(|v| v.as_array())
            .expect("field_violations must be present");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["field"], "server");
        assert_eq!(violations[0]["reason"], field::REQUIRED);
        assert!(
            violations[0]["description"]
                .as_str()
                .unwrap()
                .contains("missing required field"),
        );
    }

    #[test]
    fn validation_without_field_uses_format_variant() {
        // Sites that don't know which field failed (the legacy bulk
        // case) emit the canonical Format variant so the wire body
        // doesn't lie about which field was bad.
        let err = DomainError::validation("alias must be 1-63 chars");
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
        // No field_violations array — this is the Format variant.
        assert!(
            p.context.get("field_violations").is_none()
                || p.context["field_violations"].as_array().unwrap().is_empty(),
            "expected no field_violations on Format variant, got {:?}",
            p.context,
        );
        // The top-level detail surfaces the human-readable message.
        assert!(
            p.detail.contains("alias must be 1-63 chars"),
            "expected detail to carry the validation message, got {:?}",
            p.detail,
        );
    }

    #[test]
    fn conflict_error_produces_409() {
        let err = DomainError::Conflict {
            entity: "upstream",
            resource: "my-alias".into(),
            detail: "alias already exists".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(p.problem_type, ALREADY_EXISTS_TYPE);
        assert_eq!(p.context["resource_name"], "my-alias");
        assert_eq!(p.title, "Already Exists");
    }

    #[test]
    fn rate_limit_exceeded_produces_429() {
        let err = DomainError::RateLimitExceeded {
            detail: "rate limit exceeded for upstream".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat/completions".into(),
            retry_after_secs: Some(30),
            limit: Some(100),
            remaining: Some(0),
            reset_epoch: Some(1706626800),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 429);
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
    }

    #[test]
    fn not_found_produces_404() {
        let err = DomainError::NotFound {
            entity: "route",
            id: uuid::Uuid::nil(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
    }

    #[test]
    fn plugin_not_found_dispatches_on_gts_prefix() {
        let auth = DomainError::PluginNotFound {
            gts_id: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1").into(),
            detail: "auth plugin not registered".into(),
        };
        let p: Problem = auth.into_test_problem();
        assert_eq!(p.status, 404);
        assert_eq!(p.context["resource_type"], gts::AUTH_PLUGIN_SCHEMA);
        assert_eq!(
            p.context["resource_name"],
            gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1")
        );

        let guard = DomainError::PluginNotFound {
            gts_id: gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1").into(),
            detail: "guard plugin not registered".into(),
        };
        let p: Problem = guard.into_test_problem();
        assert_eq!(p.context["resource_type"], gts::GUARD_PLUGIN_SCHEMA);

        let xform = DomainError::PluginInUse {
            gts_id: gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1").into(),
            detail: "transform plugin in use".into(),
        };
        let p: Problem = xform.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(p.context["resource_type"], gts::TRANSFORM_PLUGIN_SCHEMA);
    }

    #[test]
    fn service_unavailable_emissions_carry_retry_after_seconds() {
        // Schema in categories/14-service-unavailable.md requires
        // `retry_after_seconds` on every service_unavailable response;
        // verify each oagw → service_unavailable path populates it with
        // the per-failure-mode default.
        fn assert_retry(err: DomainError, expected: u64, label: &str) {
            let p: Problem = err.into_test_problem();
            assert_eq!(p.status, 503, "{label} should map to 503");
            assert_eq!(
                p.context["retry_after_seconds"].as_u64(),
                Some(expected),
                "{label} retry_after_seconds mismatch",
            );
        }

        let i = || "/i".to_string();
        assert_retry(
            DomainError::DownstreamError {
                detail: "x".into(),
                instance: i(),
            },
            RETRY_AFTER_TRANSIENT_SECS,
            "DownstreamError",
        );
        assert_retry(
            DomainError::ProtocolError {
                detail: "x".into(),
                instance: i(),
            },
            RETRY_AFTER_TRANSIENT_SECS,
            "ProtocolError",
        );
        assert_retry(
            DomainError::UpstreamDisabled {
                alias: "alias".into(),
            },
            RETRY_AFTER_ADMIN_SECS,
            "UpstreamDisabled",
        );
        assert_retry(
            DomainError::StreamAborted {
                detail: "x".into(),
                instance: i(),
            },
            RETRY_AFTER_TRANSIENT_SECS,
            "StreamAborted",
        );
        assert_retry(
            DomainError::LinkUnavailable {
                detail: "x".into(),
                instance: i(),
            },
            RETRY_AFTER_LINK_SECS,
            "LinkUnavailable",
        );
        assert_retry(
            DomainError::CircuitBreakerOpen {
                detail: "x".into(),
                instance: i(),
            },
            RETRY_AFTER_CIRCUIT_BREAKER_SECS,
            "CircuitBreakerOpen",
        );
        assert_retry(
            DomainError::GuardRejected {
                status: 503,
                error_code: "UNAVAILABLE".into(),
                detail: "x".into(),
                instance: i(),
                resource_id: None,
            },
            RETRY_AFTER_GUARD_SECS,
            "GuardRejected 503",
        );
    }

    #[test]
    fn payload_too_large_now_maps_to_400() {
        let err = DomainError::PayloadTooLarge {
            detail: "request body exceeds 100MB limit".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
        };
        let p: Problem = err.into_test_problem();
        // ⚠ wire change accepted in the migration plan: 413 → 400.
        assert_eq!(p.status, 400);
    }

    #[test]
    fn downstream_error_now_maps_to_503() {
        let err = DomainError::DownstreamError {
            detail: "upstream connection refused".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
        };
        let p: Problem = err.into_test_problem();
        // ⚠ wire change accepted in the migration plan: 502 → 503.
        assert_eq!(p.status, 503);
    }

    #[test]
    fn protocol_error_now_maps_to_503() {
        let err = DomainError::ProtocolError {
            detail: "upstream HTTP/2 error".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 503);
    }

    #[test]
    fn stream_aborted_now_maps_to_503() {
        let err = DomainError::StreamAborted {
            detail: "upstream stream read error".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 503);
    }

    #[test]
    fn all_error_types_produce_valid_json() {
        let errors: Vec<DomainError> = vec![
            DomainError::Validation {
                field: "",
                reason: field::VALIDATION,
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::Conflict {
                entity: "upstream",
                resource: "test".into(),
                detail: "test".into(),
            },
            DomainError::MissingTargetHost {
                instance: "/test".into(),
            },
            DomainError::InvalidTargetHost {
                instance: "/test".into(),
            },
            DomainError::UnknownTargetHost {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::AuthenticationFailed {
                reason: reason::auth::PLUGIN_FAILED,
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::NotFound {
                entity: "route",
                id: uuid::Uuid::nil(),
            },
            DomainError::PayloadTooLarge {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::RateLimitExceeded {
                detail: "test".into(),
                instance: "/test".into(),
                retry_after_secs: None,
                limit: None,
                remaining: None,
                reset_epoch: None,
            },
            DomainError::SecretNotFound {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::SecretRefNotAccessible {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::DownstreamError {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::ProtocolError {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::UpstreamDisabled {
                alias: "test".into(),
            },
            DomainError::ConnectionTimeout {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::RequestTimeout {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::Internal {
                message: "test".into(),
            },
            DomainError::GuardRejected {
                status: 400,
                error_code: "MISSING_HEADER".into(),
                detail: "test".into(),
                instance: "/test".into(),
                resource_id: None,
            },
            DomainError::CorsOriginNotAllowed {
                origin: "https://evil.com".into(),
                instance: "/test".into(),
            },
            DomainError::CorsMethodNotAllowed {
                method: "DELETE".into(),
                instance: "/test".into(),
            },
            DomainError::StreamAborted {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::LinkUnavailable {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::CircuitBreakerOpen {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::IdleTimeout {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::PluginNotFound {
                gts_id: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1").into(),
                detail: "test".into(),
            },
            DomainError::PluginInUse {
                gts_id: gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1").into(),
                detail: "test".into(),
            },
            DomainError::Forbidden {
                reason: reason::permission::AUTHZ_DENIED.into(),
                detail: "test".into(),
            },
        ];
        for err in errors {
            let p: Problem = err.into_test_problem();
            let json = serde_json::to_string(&p).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(parsed.get("type").is_some());
            assert!(parsed.get("status").is_some());
            assert!(parsed.get("title").is_some());
            assert!(parsed.get("detail").is_some());
            assert!(parsed.get("context").is_some());
        }
    }

    #[test]
    fn domain_error_to_problem_fills_missing_instance() {
        let err = DomainError::NotFound {
            entity: "upstream",
            id: uuid::Uuid::nil(),
        };
        let p = domain_error_to_problem(err, "/oagw/v1/upstreams/123");
        assert_eq!(p.instance.as_deref(), Some("/oagw/v1/upstreams/123"));
    }

    #[test]
    fn domain_error_to_problem_overrides_instance() {
        // `domain_error_to_problem` always sets the instance to the
        // supplied request URI; per-variant `instance: String` fields are
        // ignored when the handler plumbs the request URI through.
        let err = DomainError::Validation {
            field: "",
            reason: field::VALIDATION,
            detail: "bad input".into(),
            instance: "/oagw/v1/upstreams".into(),
        };
        let p = domain_error_to_problem(err, "/fallback");
        assert_eq!(p.instance.as_deref(), Some("/fallback"));
    }

    #[test]
    fn guard_rejected_4xx_passes_through() {
        let err = DomainError::GuardRejected {
            status: 403,
            error_code: "FORBIDDEN".into(),
            detail: "test".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 403);
    }

    #[test]
    fn guard_rejected_5xx_passes_through() {
        let err = DomainError::GuardRejected {
            status: 503,
            error_code: "UNAVAILABLE".into(),
            detail: "test".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 503);
    }

    #[test]
    fn guard_rejected_2xx_falls_back_to_400() {
        let err = DomainError::GuardRejected {
            status: 200,
            error_code: "OK".into(),
            detail: "test".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
    }

    #[test]
    fn guard_rejected_3xx_falls_back_to_400() {
        let err = DomainError::GuardRejected {
            status: 301,
            error_code: "REDIRECT".into(),
            detail: "test".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
    }

    #[test]
    fn guard_rejected_invalid_status_falls_back_to_400() {
        let err = DomainError::GuardRejected {
            status: 999,
            error_code: "INVALID".into(),
            detail: "test".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
    }

    #[test]
    fn guard_rejected_404_with_resource_routes_to_not_found() {
        let err = DomainError::GuardRejected {
            status: 404,
            error_code: "MISSING".into(),
            detail: "downstream object missing".into(),
            instance: "/test".into(),
            resource_id: Some("widget-42".into()),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
        assert_eq!(p.context["resource_name"], "widget-42");
        assert_eq!(p.context["resource_type"], gts::GUARD_PLUGIN_SCHEMA);
    }

    #[test]
    fn guard_rejected_404_without_resource_falls_back_to_failed_precondition() {
        let err = DomainError::GuardRejected {
            status: 404,
            error_code: "STATE_DRIFT".into(),
            detail: "policy precondition not met".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~")
        );
        // resource_name must be absent — guard didn't supply an id.
        assert!(p.context.get("resource_name").is_none());
    }

    #[test]
    fn secret_ref_not_accessible_maps_to_failed_precondition_400() {
        // A state precondition (the secret may be provisioned later), NOT
        // invalid_argument: in-process provisioning callers rely on the
        // canonical category to classify the failure as retryable.
        let err = DomainError::SecretRefNotAccessible {
            detail: "secret_ref 'cred://missing' is not accessible to this tenant".into(),
            instance: "/test".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 400);
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~")
        );
    }

    #[test]
    fn guard_rejected_409_with_resource_routes_to_already_exists() {
        let err = DomainError::GuardRejected {
            status: 409,
            error_code: "DUPLICATE".into(),
            detail: "duplicate detected".into(),
            instance: "/test".into(),
            resource_id: Some("invoice-7".into()),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(p.problem_type, ALREADY_EXISTS_TYPE);
        assert_eq!(p.context["resource_name"], "invoice-7");
        assert_eq!(p.context["resource_type"], gts::GUARD_PLUGIN_SCHEMA);
    }

    #[test]
    fn guard_rejected_409_without_resource_falls_back_to_aborted() {
        let err = DomainError::GuardRejected {
            status: 409,
            error_code: "CONCURRENCY".into(),
            detail: "rate-limited concurrent decision".into(),
            instance: "/test".into(),
            resource_id: None,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 409);
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.aborted.v1~")
        );
        assert!(p.context.get("resource_name").is_none());
    }

    #[test]
    fn authentication_failed_carries_structured_reason_on_wire() {
        // m1: AUTH_PLUGIN_NOT_FOUND / _FAILED / _INTERNAL must reach the
        // wire context so clients can branch programmatically rather than
        // parsing the human-readable detail.
        for auth_reason in [
            reason::auth::PLUGIN_NOT_FOUND,
            reason::auth::PLUGIN_FAILED,
            reason::auth::PLUGIN_INTERNAL,
        ] {
            let err = DomainError::AuthenticationFailed {
                reason: auth_reason,
                detail: "plugin failed".into(),
                instance: "/test".into(),
            };
            let p: Problem = err.into_test_problem();
            assert_eq!(p.status, 401);
            assert_eq!(
                p.problem_type,
                gts_uri!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~")
            );
            assert_eq!(
                p.context["reason"], auth_reason,
                "reason must reach the wire",
            );
        }
    }

    #[test]
    fn forbidden_carries_pep_error_code_as_reason() {
        // m2: when EnforcerError::Denied carries a deny_reason.error_code,
        // it lands on the wire as `permission_denied.reason` — the structured
        // PEP code is no longer collapsed into the prose detail.
        let err = DomainError::forbidden_with_reason(
            reason::permission::TENANT_BOUNDARY_VIOLATION,
            "subject not allowed to act outside its tenant",
        );
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, 403);
        assert_eq!(
            p.context["reason"],
            reason::permission::TENANT_BOUNDARY_VIOLATION
        );
    }

    #[test]
    fn forbidden_default_reason_is_authz_denied() {
        // The convenience constructor still works for callers that don't
        // have a more specific code — the default is the stable
        // `AUTHZ_DENIED` taxonomy member.
        let err = DomainError::forbidden("policy denied");
        let p: Problem = err.into_test_problem();
        assert_eq!(p.context["reason"], reason::permission::AUTHZ_DENIED);
    }

    #[test]
    fn error_response_sets_gateway_header() {
        let err = DomainError::NotFound {
            entity: "route",
            id: uuid::Uuid::nil(),
        };
        let resp = error_response(err);
        assert_eq!(
            resp.headers().get("x-oagw-error-source").unwrap(),
            "gateway"
        );
    }
}
