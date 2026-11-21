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
use toolkit_canonical_errors::builder::{HasReason, OptionalReason};
use toolkit_canonical_errors::{
    CanonicalError, Http, Problem, ResourceErrorBuilder, ServiceUnavailableBuilder, resource_error,
};

use crate::domain::error::{DeniedOn, Dispatch, DomainError};
use crate::domain::gts_helpers as gts;
use oagw_sdk::api::ErrorSource;
use oagw_sdk::reason::{timeout::TimeoutReason, unavailable::UnavailableReason};
use oagw_sdk::{field, precondition, quota, reason};

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

/// Upstream disabled by configuration — only an operator can enable it again,
/// so the hint is long.
const RETRY_AFTER_ADMIN_SECS: u64 = 300;

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

            // A state precondition: the row changes only with its
            // types-registry instance, so retrying the request cannot help.
            DomainError::RegistryManaged { entity, id } => {
                let detail = format!(
                    "{entity} {id} is managed by the types registry; \
                     change its registry instance instead"
                );
                match entity {
                    "upstream" => OagwUpstreamError::failed_precondition()
                        .with_precondition_violation(
                            precondition::MANAGED_BY,
                            detail,
                            precondition::REGISTRY_MANAGED,
                        )
                        .create(),
                    "route" => OagwRouteError::failed_precondition()
                        .with_precondition_violation(
                            precondition::MANAGED_BY,
                            detail,
                            precondition::REGISTRY_MANAGED,
                        )
                        .create(),
                    _ => OagwProxyError::failed_precondition()
                        .with_precondition_violation(
                            precondition::MANAGED_BY,
                            detail,
                            precondition::REGISTRY_MANAGED,
                        )
                        .create(),
                }
            }

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
                    .with_override(Http::status_code(413))
                    .create()
            }

            // The limiter's retry hint rides on the quota violation, so the
            // wire `Retry-After` header (set by `Problem::into_response`) and
            // the SDK's `RateLimited { retry_after_secs }` carry the same value.
            DomainError::RateLimitExceeded {
                detail,
                retry_after_secs,
                ..
            } => {
                let mut builder = OagwProxyError::resource_exhausted(detail.clone())
                    .with_quota_violation(quota::RATE_LIMIT, detail);
                if let Some(secs) = retry_after_secs {
                    builder = builder.with_quota_violation_retry_after_seconds(secs);
                }
                builder.create()
            }

            // The secret is missing or not shared with the tenant: the
            // credential is absent, as for any other auth failure. The detail
            // (config key and credential store error kind) stays off the
            // wire; the auth plugin step already warned with it and the
            // request's identifiers.
            DomainError::SecretNotFound { .. } => CanonicalError::unauthenticated()
                .with_reason(reason::auth::CREDENTIAL_NOT_FOUND)
                .create(),

            // A state precondition, not a malformed request: the referenced
            // secret is not provisioned yet or not shared. Kept distinct from
            // `Validation`/`invalid_argument` so in-process provisioning
            // callers can classify it as retryable (HTTP status is 400 for
            // both, but the canonical category differs).
            DomainError::SecretRefNotAccessible { field, detail, .. } => {
                OagwUpstreamError::failed_precondition()
                    .with_precondition_violation(field, detail, precondition::STATE)
                    .create()
            }

            // The credential store is down; the upstream was not contacted.
            DomainError::CredentialStoreUnavailable {
                detail,
                retry_after,
                ..
            } => {
                // `secret_ref::resolve` or the auth plugin step already
                // warned with the request's identifiers; this is only the
                // mapping step.
                tracing::debug!(reason = %detail, "OAGW credential store unavailable");
                // CredStore's hint, rounded up to whole seconds (at least 1).
                let retry_after_secs =
                    retry_after.map_or(RETRY_AFTER_TRANSIENT_SECS, retry_after_seconds);
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(retry_after_secs)
                    .with_typed_reason(UnavailableReason::CredentialStoreUnavailable)
                    .create()
            }

            // The identity provider's token endpoint is down; the upstream
            // was not contacted.
            DomainError::IdpUnavailable { detail, .. } => {
                // The auth plugin step already warned with the request's
                // identifiers; this is only the mapping step.
                tracing::debug!(reason = %detail, "OAGW OAuth2 identity provider unavailable");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
                    .with_typed_reason(UnavailableReason::IdentityProviderUnavailable)
                    .create()
            }

            // The auth step failed before anything was sent upstream.
            DomainError::IdpInvalidResponse { detail, .. } => {
                // The auth plugin step already warned with the request's
                // identifiers; this is only the mapping step.
                tracing::debug!(reason = %detail, "OAGW identity provider sent an invalid response");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
                    .with_typed_reason(UnavailableReason::RequestNotSent)
                    .with_override(Http::status_code(502))
                    .create()
            }

            // Transport failures: the status follows the variant, the
            // `reason` the dispatch stage the engine error kind set.
            DomainError::DownstreamError {
                detail, dispatch, ..
            } => {
                tracing::warn!(reason = %detail, ?dispatch, "OAGW downstream error");
                bad_gateway(dispatch)
            }

            DomainError::ProtocolError {
                detail, dispatch, ..
            } => {
                tracing::warn!(reason = %detail, ?dispatch, "OAGW upstream protocol error");
                bad_gateway(dispatch)
            }

            DomainError::UpstreamDisabled { alias } => {
                tracing::debug!(upstream = %alias, "OAGW upstream is disabled");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_ADMIN_SECS)
                    .with_typed_reason(UnavailableReason::UpstreamDisabled)
                    .create()
            }

            DomainError::ConnectionTimeout {
                detail, dispatch, ..
            }
            | DomainError::RequestTimeout {
                detail, dispatch, ..
            } => OagwProxyError::deadline_exceeded(detail)
                .with_typed_reason(dispatch.timeout_reason())
                .create(),

            DomainError::Internal { message } => {
                // The canonical error middleware logs `Internal::description`
                // via the extension path, so a per-arm `tracing::error!`
                // would be redundant.
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

            DomainError::StreamAborted {
                detail, dispatch, ..
            } => {
                tracing::warn!(reason = %detail, ?dispatch, "OAGW upstream stream aborted");
                bad_gateway(dispatch)
            }

            DomainError::LinkUnavailable {
                detail, dispatch, ..
            } => {
                tracing::warn!(reason = %detail, ?dispatch, "OAGW upstream link unavailable");
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(RETRY_AFTER_LINK_SECS)
                    .with_typed_reason(dispatch.unavailable_reason())
                    .create()
            }

            DomainError::CircuitBreakerOpen {
                detail,
                retry_after,
                ..
            } => {
                // The toolkit logs the transition and counts rejections.
                tracing::debug!(reason = %detail, "OAGW circuit breaker open");
                // Until the breaker lets a probe through. Nothing was sent.
                CanonicalError::service_unavailable()
                    .with_retry_after_seconds(retry_after_seconds(retry_after))
                    .with_typed_reason(UnavailableReason::RequestNotSent)
                    .create()
            }

            // The upstream went quiet after the exchange started.
            DomainError::IdleTimeout { detail, .. } => OagwProxyError::deadline_exceeded(detail)
                .with_typed_reason(TimeoutReason::RequestMayHaveBeenSent)
                .create(),

            // The `gts_id` prefix encodes the plugin kind, so dispatch
            // through the matching per-kind resource scope. The wire
            // `resource_name` carries the actual plugin GTS identifier.
            DomainError::PluginNotFound { gts_id, detail } => {
                plugin_not_found_to_canonical(gts_id, detail)
            }

            DomainError::PluginInUse { gts_id, detail } => {
                plugin_in_use_to_canonical(gts_id, detail)
            }

            DomainError::Forbidden {
                reason,
                detail,
                entity,
            } => {
                tracing::debug!(reason = %reason, detail = %detail, ?entity, "OAGW authorization denied");
                match entity {
                    DeniedOn::Upstream => OagwUpstreamError::permission_denied()
                        .with_reason(reason)
                        .create(),
                    DeniedOn::Route => OagwRouteError::permission_denied()
                        .with_reason(reason)
                        .create(),
                    DeniedOn::Proxy => OagwProxyError::permission_denied()
                        .with_reason(reason)
                        .create(),
                }
            }
        }
    }
}

/// `with_reason` that takes the SDK enum for the category: the 502/503
/// builder takes only an [`UnavailableReason`], the 504 builder only a
/// [`TimeoutReason`], so a reason from the wrong category does not compile.
/// Emit named variants; `Unknown` is for reading unfamiliar codes.
trait WithTypedReason {
    type Reason;
    type Output;

    fn with_typed_reason(self, reason: Self::Reason) -> Self::Output;
}

impl WithTypedReason for ServiceUnavailableBuilder {
    type Reason = UnavailableReason;
    type Output = Self;

    fn with_typed_reason(self, reason: UnavailableReason) -> Self {
        self.with_reason(reason.as_wire())
    }
}

impl<Resource> WithTypedReason for ResourceErrorBuilder<Resource, OptionalReason> {
    type Reason = TimeoutReason;
    type Output = ResourceErrorBuilder<Resource, HasReason>;

    fn with_typed_reason(self, reason: TimeoutReason) -> Self::Output {
        self.with_reason(reason.as_wire())
    }
}

/// A 502 for a failed upstream exchange: canonical `service_unavailable`
/// with the transport override, the short retry hint and the dispatch
/// reason.
fn bad_gateway(dispatch: Dispatch) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_retry_after_seconds(RETRY_AFTER_TRANSIENT_SECS)
        .with_typed_reason(dispatch.unavailable_reason())
        .with_override(Http::status_code(502))
        .create()
}

/// A retry hint in whole seconds, rounded up and at least 1.
fn retry_after_seconds(d: std::time::Duration) -> u64 {
    (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
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
                .with_precondition_violation(error_code, detail, precondition::STATE)
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
/// is converted into `X-RateLimit-*` wire headers and the detail is
/// sanitised to avoid leaking internal key structure (resource IDs, tenant
/// IDs, scope) in the body. `Retry-After` comes from the quota violation's
/// retry hint via `Problem::into_response`.
///
/// `instance` / `trace_id` are NOT set here — the canonical error
/// middleware (`toolkit::api::canonical_error_middleware`) injects them on
/// the response on the way out via the same router this helper feeds into.
pub fn error_response(err: DomainError) -> Response {
    let rate_limit_meta = match &err {
        DomainError::RateLimitExceeded {
            limit,
            remaining,
            reset_epoch,
            ..
        } => Some((*limit, *remaining, *reset_epoch)),
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

    // `Retry-After` is not set here: the retry hint travels on the quota
    // violation and `Problem::into_response` derives the header from it.
    if let Some((limit, remaining, reset_epoch)) = rate_limit_meta {
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
    use uuid::Uuid;

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
        assert_eq!(p.status, Some(400));
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
        assert_eq!(p.status, Some(400));
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
        assert_eq!(p.status, Some(409));
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
        assert_eq!(p.status, Some(429));
        assert_eq!(p.problem_type, RESOURCE_EXHAUSTED_TYPE);
        assert_eq!(
            p.context["violations"][0]["retry_after_seconds"].as_u64(),
            Some(30),
        );
    }

    fn rate_limit_error(retry_after_secs: Option<u64>) -> DomainError {
        DomainError::RateLimitExceeded {
            detail: "rate limit exceeded for upstream".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat/completions".into(),
            retry_after_secs,
            limit: Some(100),
            remaining: Some(0),
            reset_epoch: Some(1706626800),
        }
    }

    /// The SDK projection and the REST `Retry-After` header must agree on
    /// the limiter's retry hint.
    #[test]
    fn rate_limit_retry_hint_reaches_sdk_and_wire() {
        for hint in [Some(30), None] {
            let sdk =
                oagw_sdk::ServiceGatewayError::from(CanonicalError::from(rate_limit_error(hint)));
            assert!(
                matches!(
                    sdk,
                    oagw_sdk::ServiceGatewayError::RateLimited { retry_after_secs }
                        if retry_after_secs == hint
                ),
                "hint {hint:?}: got {sdk:?}",
            );

            let resp = error_response(rate_limit_error(hint));
            assert_eq!(resp.status(), 429);
            let header = resp
                .headers()
                .get("retry-after")
                .map(|v| v.to_str().unwrap().to_owned());
            assert_eq!(header, hint.map(|s| s.to_string()), "hint {hint:?}");
            assert_eq!(
                resp.headers().get_all("retry-after").iter().count(),
                usize::from(hint.is_some())
            );
            assert_eq!(resp.headers().get("x-ratelimit-limit").unwrap(), "100");
        }
    }

    /// Clients that rebuild the error from the problem body see the hint too.
    #[test]
    fn rate_limit_retry_hint_survives_problem_round_trip() {
        let p: Problem = rate_limit_error(Some(30)).into_test_problem();
        let rebuilt = CanonicalError::try_from(p).expect("problem should round-trip");
        assert!(matches!(
            oagw_sdk::ServiceGatewayError::from(rebuilt),
            oagw_sdk::ServiceGatewayError::RateLimited {
                retry_after_secs: Some(30)
            }
        ));
    }

    #[test]
    fn not_found_produces_404() {
        let err = DomainError::NotFound {
            entity: "route",
            id: uuid::Uuid::nil(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(404));
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
    }

    #[test]
    fn plugin_not_found_dispatches_on_gts_prefix() {
        let auth = DomainError::PluginNotFound {
            gts_id: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1").into(),
            detail: "auth plugin not registered".into(),
        };
        let p: Problem = auth.into_test_problem();
        assert_eq!(p.status, Some(404));
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
        assert_eq!(p.status, Some(409));
        assert_eq!(p.context["resource_type"], gts::TRANSFORM_PLUGIN_SCHEMA);
    }

    #[test]
    fn service_unavailable_emissions_carry_retry_after_seconds() {
        // categories/14-service-unavailable.md requires `retry_after_seconds`
        // on every service_unavailable emission. Wire status splits by
        // attribution: upstream faults override to 502, gateway/admin
        // unavailability stays at the 503 default.
        fn assert_retry(err: DomainError, expected_status: u16, expected: u64, label: &str) {
            let p: Problem = err.into_test_problem();
            assert_eq!(
                p.status,
                Some(expected_status),
                "{label} should map to {expected_status}",
            );
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
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            502,
            RETRY_AFTER_TRANSIENT_SECS,
            "DownstreamError",
        );
        assert_retry(
            DomainError::ProtocolError {
                detail: "x".into(),
                instance: i(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            502,
            RETRY_AFTER_TRANSIENT_SECS,
            "ProtocolError",
        );
        assert_retry(
            DomainError::UpstreamDisabled {
                alias: "alias".into(),
            },
            503,
            RETRY_AFTER_ADMIN_SECS,
            "UpstreamDisabled",
        );
        assert_retry(
            DomainError::StreamAborted {
                detail: "x".into(),
                instance: i(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            502,
            RETRY_AFTER_TRANSIENT_SECS,
            "StreamAborted",
        );
        assert_retry(
            DomainError::LinkUnavailable {
                detail: "x".into(),
                instance: i(),
                dispatch: crate::domain::error::Dispatch::NotSent,
            },
            503,
            RETRY_AFTER_LINK_SECS,
            "LinkUnavailable",
        );
        assert_retry(
            DomainError::CircuitBreakerOpen {
                detail: "x".into(),
                instance: i(),
                retry_after: std::time::Duration::from_millis(12_300),
            },
            503,
            13,
            "CircuitBreakerOpen",
        );
        assert_retry(
            DomainError::CircuitBreakerOpen {
                detail: "x".into(),
                instance: i(),
                retry_after: std::time::Duration::ZERO,
            },
            503,
            1,
            "CircuitBreakerOpen about to probe",
        );
        assert_retry(
            DomainError::GuardRejected {
                status: 503,
                error_code: "UNAVAILABLE".into(),
                detail: "x".into(),
                instance: i(),
                resource_id: None,
            },
            503,
            RETRY_AFTER_GUARD_SECS,
            "GuardRejected 503",
        );
        assert_retry(
            DomainError::CredentialStoreUnavailable {
                detail: "x".into(),
                retry_after: None,
                instance: i(),
            },
            503,
            RETRY_AFTER_TRANSIENT_SECS,
            "CredentialStoreUnavailable without a hint",
        );
        assert_retry(
            DomainError::CredentialStoreUnavailable {
                detail: "x".into(),
                retry_after: Some(std::time::Duration::from_secs(9)),
                instance: i(),
            },
            503,
            9,
            "CredentialStoreUnavailable with a hint",
        );
        assert_retry(
            DomainError::CredentialStoreUnavailable {
                detail: "x".into(),
                retry_after: Some(std::time::Duration::from_millis(1)),
                instance: i(),
            },
            503,
            1,
            "CredentialStoreUnavailable with a sub-second hint",
        );
        assert_retry(
            DomainError::IdpUnavailable {
                detail: "x".into(),
                instance: i(),
            },
            503,
            RETRY_AFTER_TRANSIENT_SECS,
            "IdpUnavailable",
        );
    }

    #[test]
    fn payload_too_large_maps_to_413() {
        let err = DomainError::PayloadTooLarge {
            detail: "request body exceeds 100MB limit".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(413));
        // Canonical category is unchanged — only the wire status is overridden.
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.out_of_range.v1~")
        );
    }

    #[test]
    fn downstream_error_maps_to_502() {
        let err = DomainError::DownstreamError {
            detail: "upstream connection refused".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(502));
        // Canonical category is unchanged — only the wire status is overridden.
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~")
        );
    }

    #[test]
    fn protocol_error_maps_to_502() {
        let err = DomainError::ProtocolError {
            detail: "upstream HTTP/2 error".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(502));
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~")
        );
    }

    #[test]
    fn stream_aborted_maps_to_502() {
        let err = DomainError::StreamAborted {
            detail: "upstream stream read error".into(),
            instance: "/oagw/v1/proxy/api.openai.com/v1/chat".into(),
            dispatch: crate::domain::error::Dispatch::MaybeSent,
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(502));
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~")
        );
    }

    const SERVICE_UNAVAILABLE_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~");
    const DEADLINE_EXCEEDED_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~");
    const UNAUTHENTICATED_TYPE: &str =
        gts_uri!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~");
    const INTERNAL_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.internal.v1~");

    /// The JSON body the conversion produces (no `instance` / `trace_id`:
    /// the middleware adds those).
    fn body(err: DomainError) -> serde_json::Value {
        serde_json::to_value(err.into_test_problem()).unwrap()
    }

    fn unavailable_body(status: u16, retry: u64, reason: &str) -> serde_json::Value {
        serde_json::json!({
            "type": SERVICE_UNAVAILABLE_TYPE,
            "title": "Service Unavailable",
            "status": status,
            "detail": "Service temporarily unavailable",
            "context": { "retry_after_seconds": retry, "reason": reason },
        })
    }

    /// Each gateway 502/503/504 from the upstream exchange carries the
    /// dispatch reason its stage set, whatever the variant: the full
    /// problem body for every transport variant and both stages.
    #[test]
    fn transport_errors_carry_the_dispatch_reason() {
        use crate::domain::error::Dispatch::{MaybeSent, NotSent};
        use reason::unavailable::{REQUEST_MAY_HAVE_BEEN_SENT, REQUEST_NOT_SENT};

        for (dispatch, wire) in [
            (NotSent, REQUEST_NOT_SENT),
            (MaybeSent, REQUEST_MAY_HAVE_BEEN_SENT),
        ] {
            let d = || "upstream connection refused".to_string();
            let i = || "/oagw/v1/proxy/a/b".to_string();
            for err in [
                DomainError::DownstreamError {
                    detail: d(),
                    instance: i(),
                    dispatch,
                },
                DomainError::ProtocolError {
                    detail: d(),
                    instance: i(),
                    dispatch,
                },
                DomainError::StreamAborted {
                    detail: d(),
                    instance: i(),
                    dispatch,
                },
            ] {
                assert_eq!(
                    body(err),
                    unavailable_body(502, RETRY_AFTER_TRANSIENT_SECS, wire)
                );
            }
            assert_eq!(
                body(DomainError::LinkUnavailable {
                    detail: d(),
                    instance: i(),
                    dispatch,
                }),
                unavailable_body(503, RETRY_AFTER_LINK_SECS, wire)
            );
            for err in [
                DomainError::ConnectionTimeout {
                    detail: "upstream connection timed out".into(),
                    instance: i(),
                    dispatch,
                },
                DomainError::RequestTimeout {
                    detail: "upstream connection timed out".into(),
                    instance: i(),
                    dispatch,
                },
            ] {
                assert_eq!(
                    body(err),
                    serde_json::json!({
                        "type": DEADLINE_EXCEEDED_TYPE,
                        "title": "Deadline Exceeded",
                        "status": 504,
                        "detail": "upstream connection timed out",
                        "context": {
                            "resource_type": gts::PROXY_SCHEMA,
                            "reason": wire,
                        },
                    })
                );
            }
        }
    }

    /// An open circuit sends nothing; a disabled upstream says so, with a
    /// long retry hint because only an operator can enable it again.
    #[test]
    fn breaker_and_disabled_upstream_bodies() {
        assert_eq!(
            body(DomainError::CircuitBreakerOpen {
                detail: "circuit open".into(),
                instance: "/i".into(),
                retry_after: std::time::Duration::from_secs(4),
            }),
            unavailable_body(503, 4, reason::unavailable::REQUEST_NOT_SENT)
        );
        assert_eq!(RETRY_AFTER_ADMIN_SECS, 300);
        assert_eq!(
            body(DomainError::upstream_disabled("api.example.com")),
            unavailable_body(503, 300, reason::unavailable::UPSTREAM_DISABLED)
        );
        let resp = error_response(DomainError::upstream_disabled("api.example.com"));
        assert_eq!(resp.status(), 503);
        assert_eq!(resp.headers()["retry-after"], "300");
    }

    /// The credential-failure mapping: a missing secret is 401 with its own
    /// reason (it was 500), a credential store outage 503 with the store's
    /// hint, an identity provider outage 503 with its own reason and the
    /// transient hint, an invalid identity provider answer 502. The wire
    /// never carries the config key, the reference or the detail.
    #[test]
    fn credential_failures_have_one_mapping() {
        let missing = body(DomainError::SecretNotFound {
            detail: "secret referenced by auth.config.secret_ref not found".into(),
            instance: "/i".into(),
        });
        assert_eq!(missing["type"], UNAUTHENTICATED_TYPE);
        assert_eq!(missing["status"], 401);
        assert_eq!(
            missing["context"],
            serde_json::json!({ "reason": reason::auth::CREDENTIAL_NOT_FOUND })
        );
        assert!(!missing.to_string().contains("secret_ref"), "{missing}");

        assert_eq!(
            body(DomainError::CredentialStoreUnavailable {
                detail: "credential store unavailable for auth.config.secret_ref".into(),
                retry_after: Some(std::time::Duration::from_millis(6_500)),
                instance: "/i".into(),
            }),
            unavailable_body(503, 7, reason::unavailable::CREDENTIAL_STORE_UNAVAILABLE)
        );
        assert_eq!(
            body(DomainError::IdpUnavailable {
                detail: "identity provider unavailable: token endpoint returned 503".into(),
                instance: "/i".into(),
            }),
            unavailable_body(
                503,
                RETRY_AFTER_TRANSIENT_SECS,
                reason::unavailable::IDENTITY_PROVIDER_UNAVAILABLE
            )
        );
        assert_eq!(
            body(DomainError::IdpInvalidResponse {
                detail: "invalid identity provider response: unsupported token type".into(),
                instance: "/i".into(),
            }),
            unavailable_body(
                502,
                RETRY_AFTER_TRANSIENT_SECS,
                reason::unavailable::REQUEST_NOT_SENT
            )
        );
        let misconfigured = body(DomainError::internal(
            "auth plugin misconfigured: missing client_id_ref",
        ));
        assert_eq!(misconfigured["type"], INTERNAL_TYPE);
        assert_eq!(misconfigured["status"], 500);
        assert!(
            !misconfigured.to_string().contains("client_id_ref"),
            "{misconfigured}"
        );
    }

    /// REST callers rebuild the SDK projection from the problem body: the
    /// reasons survive the round trip.
    #[test]
    fn dispatch_reasons_reach_the_sdk_projection() {
        use oagw_sdk::ServiceGatewayError as E;
        use oagw_sdk::reason::{timeout::TimeoutReason, unavailable::UnavailableReason};

        let sdk = |err: DomainError| {
            let rebuilt = CanonicalError::try_from(err.into_test_problem()).unwrap();
            E::from(rebuilt)
        };
        assert!(matches!(
            sdk(DomainError::DownstreamError {
                detail: "x".into(),
                instance: "/i".into(),
                dispatch: crate::domain::error::Dispatch::NotSent,
            }),
            E::Unavailable {
                retry_after_secs: Some(RETRY_AFTER_TRANSIENT_SECS),
                reason: Some(UnavailableReason::RequestNotSent),
            }
        ));
        assert!(matches!(
            sdk(DomainError::RequestTimeout {
                detail: "x".into(),
                instance: "/i".into(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            }),
            E::Timeout {
                reason: Some(TimeoutReason::RequestMayHaveBeenSent),
            }
        ));
        assert!(matches!(
            sdk(DomainError::upstream_disabled("a")),
            E::Unavailable {
                retry_after_secs: Some(300),
                reason: Some(UnavailableReason::UpstreamDisabled),
            }
        ));
        assert!(matches!(
            sdk(DomainError::SecretNotFound {
                detail: "x".into(),
                instance: "/i".into(),
            }),
            E::AuthFailed {
                reason: oagw_sdk::reason::auth::FailureReason::CredentialNotFound,
                ..
            }
        ));
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
            DomainError::RegistryManaged {
                entity: "route",
                id: Uuid::nil(),
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
                field: "auth.config.secret_ref",
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::DownstreamError {
                detail: "test".into(),
                instance: "/test".into(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            DomainError::ProtocolError {
                detail: "test".into(),
                instance: "/test".into(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            DomainError::UpstreamDisabled {
                alias: "test".into(),
            },
            DomainError::ConnectionTimeout {
                detail: "test".into(),
                instance: "/test".into(),
                dispatch: crate::domain::error::Dispatch::NotSent,
            },
            DomainError::RequestTimeout {
                detail: "test".into(),
                instance: "/test".into(),
                dispatch: crate::domain::error::Dispatch::MaybeSent,
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
                dispatch: crate::domain::error::Dispatch::MaybeSent,
            },
            DomainError::LinkUnavailable {
                detail: "test".into(),
                instance: "/test".into(),
                dispatch: crate::domain::error::Dispatch::NotSent,
            },
            DomainError::CircuitBreakerOpen {
                detail: "test".into(),
                instance: "/test".into(),
                retry_after: std::time::Duration::from_secs(1),
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
                entity: DeniedOn::Proxy,
            },
            DomainError::CredentialStoreUnavailable {
                detail: "test".into(),
                retry_after: None,
                instance: "/test".into(),
            },
            DomainError::IdpUnavailable {
                detail: "test".into(),
                instance: "/test".into(),
            },
            DomainError::IdpInvalidResponse {
                detail: "test".into(),
                instance: "/test".into(),
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
        assert_eq!(p.status, Some(403));
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
        assert_eq!(p.status, Some(503));
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
        assert_eq!(p.status, Some(400));
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
        assert_eq!(p.status, Some(400));
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
        assert_eq!(p.status, Some(400));
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
        assert_eq!(p.status, Some(404));
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
        assert_eq!(p.status, Some(400));
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
            field: "auth.config.secret_ref",
            detail: "secret_ref 'cred://missing' is not accessible to this tenant".into(),
            instance: "/test".into(),
        };
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(400));
        assert_eq!(
            p.problem_type,
            gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~")
        );
    }

    /// ADR-0018: PERS-OWN-001.
    #[test]
    fn registry_managed_maps_to_failed_precondition_400_per_resource() {
        // Not retryable and not a malformed request: the row changes only
        // with its types-registry instance.
        // An entity without its own scope falls back to the proxy, as for Conflict.
        let proxy = gts_id!("cf.core.oagw.proxy.v1~");
        for (entity, resource_type) in [
            ("upstream", gts::UPSTREAM_SCHEMA),
            ("route", gts::ROUTE_SCHEMA),
            ("plugin", proxy),
        ] {
            let id = Uuid::new_v4();
            let p: Problem = DomainError::registry_managed(entity, id).into_test_problem();
            assert_eq!(p.status, Some(400));
            assert_eq!(
                p.problem_type,
                gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~")
            );
            assert_eq!(p.context["resource_type"], resource_type);
            let violation = &p.context["violations"][0];
            assert_eq!(violation["type"], "REGISTRY_MANAGED");
            assert_eq!(violation["subject"], "managed_by");
            let description = violation["description"].as_str().unwrap();
            assert!(
                description.contains(&format!("{entity} {id}")),
                "{description}"
            );
        }
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
        assert_eq!(p.status, Some(409));
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
        assert_eq!(p.status, Some(409));
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
            assert_eq!(p.status, Some(401));
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
        // m2: when EnforcerError::AccessDenied(AccessDenial::Pdp { reason })
        // carries a reason.error_code, it lands on the wire as `permission_denied.reason` — the structured
        // PEP code is no longer collapsed into the prose detail.
        let err = DomainError::forbidden_with_reason(
            reason::permission::TENANT_BOUNDARY_VIOLATION,
            "subject not allowed to act outside its tenant",
        );
        let p: Problem = err.into_test_problem();
        assert_eq!(p.status, Some(403));
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
