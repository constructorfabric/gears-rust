//! OAGW SDK error surface — typed projection of [`CanonicalError`].
//!
//! The OAGW gateway emits failures as [`CanonicalError`] internally, then
//! projects them into [`ServiceGatewayError`] at the SDK boundary. The
//! projection is infallible via `From<CanonicalError>`; canonical
//! categories the SDK does not specifically model fall through to
//! [`ServiceGatewayError::Other`], which holds the full [`CanonicalError`]
//! for inspection / typed dispatch on the inner variant.
//!
//! See `docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md`
//! for the workspace pattern.
//!
//! ## What OAGW emits — consumer dispatch reference
//!
//! | Disposition | Match arm |
//! |---|---|
//! | rate-limited | [`ServiceGatewayError::RateLimited`] |
//! | timed out | [`ServiceGatewayError::Timeout`] |
//! | gateway/upstream broken, retry hint | [`ServiceGatewayError::Unavailable`] |
//! | auth failed — inspect reason | [`ServiceGatewayError::AuthFailed`] |
//! | permission denied — inspect reason | [`ServiceGatewayError::PermissionDenied`] |
//! | request body too large | [`ServiceGatewayError::PayloadTooLarge`] |
//! | `x-target-host` header issue | [`ServiceGatewayError::InvalidTargetHost`] |
//! | other field validation | [`ServiceGatewayError::Validation`] |
//! | resource missing | [`ServiceGatewayError::NotFound`] |
//! | resource conflict | [`ServiceGatewayError::AlreadyExists`] |
//! | guard precondition (no resource named) | [`ServiceGatewayError::FailedPrecondition`] |
//! | guard concurrency conflict (no resource named) | [`ServiceGatewayError::Aborted`] |
//! | internal gateway error | [`ServiceGatewayError::Internal`] |
//! | anything else (forward-compat) | [`ServiceGatewayError::Other`] |
//!
//! Consumers that need to dispatch on specific field-violation codes
//! beyond the broken-out cases can `match` against the constants in
//! [`crate::field`] inside the [`ServiceGatewayError::Validation`] arm.
//!
//! [`ServiceGatewayError::Other`] holds the full [`CanonicalError`] for
//! categories the SDK does not specifically model. Consumers that need
//! to dispatch on those (rare) cases can `match` on the inner enum:
//!
//! ```ignore
//! use toolkit_canonical_errors::CanonicalError;
//! match err {
//!     ServiceGatewayError::Other { canonical: CanonicalError::Cancelled { .. } } =>
//!         /* client disconnected */,
//!     ServiceGatewayError::Other { canonical } =>
//!         /* generic — canonical.gts_type() / canonical.detail() */,
//!     _ => /* modeled variant */,
//! }
//! ```
//!
//! ## Example
//!
//! ```ignore
//! use oagw_sdk::{reason::auth::FailureReason, ServiceGatewayError};
//!
//! match err {
//!     ServiceGatewayError::RateLimited { retry_after_secs } => /* backoff */,
//!     ServiceGatewayError::Timeout => /* retry */,
//!     ServiceGatewayError::Unavailable { retry_after_secs } => /* retry */,
//!     ServiceGatewayError::AuthFailed { reason: FailureReason::PluginInternal, .. } =>
//!         /* gateway-side auth machinery broken — transient */,
//!     ServiceGatewayError::AuthFailed { .. } => /* creds rejected */,
//!     ServiceGatewayError::PayloadTooLarge { detail } => /* user 413 */,
//!     ServiceGatewayError::Validation { field, reason, detail } =>
//!         /* generic field error — match on `reason` against oagw_sdk::field::* if needed */,
//!     _ => /* fallback */,
//! }
//! ```

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::field::TargetHostCode;
use crate::gts::Resource;
use crate::reason::auth::FailureReason as AuthFailureReason;
use crate::reason::permission::DenialReason as PermissionDenialReason;

/// Typed projection of [`CanonicalError`] for OAGW consumers.
///
/// The impl crate's `From<DomainError> for CanonicalError` is the single
/// authoritative AIP-193 mapping; this projection translates canonical
/// categories into the dispositions consumers actually dispatch on.
/// Conversion is infallible — unmodeled canonical variants fall through to
/// [`Self::Other`].
#[derive(Debug, Clone, Error)]
pub enum ServiceGatewayError {
    // ─── Retryable transient ──────────────────────────────────────────
    /// Rate limit exceeded. Backoff and retry.
    ///
    /// `retry_after_secs` carries the per-violation retry hint when the
    /// gateway knows when capacity returns (e.g. token-bucket refill window).
    /// It is `None` when the upstream rate-limit middleware did not attach
    /// one to the canonical `QuotaViolation`.
    #[error("rate limit exceeded")]
    RateLimited { retry_after_secs: Option<u64> },

    /// Connection / request / idle timeout. Retry usually helps.
    #[error("request timed out")]
    Timeout,

    /// Upstream broken, disabled, or circuit-breaker open.
    /// `retry_after_secs` carries the gateway's recommended backoff
    /// (the same value the wire `Retry-After` header carries on REST
    /// responses).
    #[error("service unavailable")]
    Unavailable { retry_after_secs: Option<u64> },

    // ─── Auth ─────────────────────────────────────────────────────────
    /// Authentication failed. Inspect `reason` to disposition between
    /// retryable gateway-side failures ([`AuthFailureReason::PluginInternal`])
    /// and user-credential failures.
    #[error("authentication failed [{reason}]: {detail}")]
    AuthFailed {
        reason: AuthFailureReason,
        detail: String,
    },

    // ─── Authorization ────────────────────────────────────────────────
    /// Permission denied by policy, tenant resolver, or CORS pre-flight.
    #[error("permission denied [{reason}]: {detail}")]
    PermissionDenied {
        reason: PermissionDenialReason,
        detail: String,
    },

    // ─── Validation ───────────────────────────────────────────────────
    /// Request body exceeded the configured size limit.
    #[error("payload too large: {detail}")]
    PayloadTooLarge { detail: String },

    /// The `x-target-host` header was missing, malformed, or did not
    /// match any registered endpoint on the upstream.
    #[error("target host header [{code:?}]: {detail}")]
    InvalidTargetHost {
        code: TargetHostCode,
        detail: String,
    },

    /// Generic field validation failure. `field` is the form field path
    /// (e.g. `"cors.allowed_origins"`, `"gts_id"`, `"path"`), `reason`
    /// is one of the [`crate::field`] constants. Consumers that need
    /// finer dispatch within validation should match `reason` against
    /// those constants.
    #[error("validation [{field}/{reason}]: {detail}")]
    Validation {
        field: String,
        reason: String,
        detail: String,
    },

    // ─── Resource lifecycle ───────────────────────────────────────────
    /// Resource not found. `resource` is the typed kind; `name` is the
    /// raw identifier (UUID for upstream/route, GTS id for plugins).
    #[error("{resource:?} not found: {name}")]
    NotFound { resource: Resource, name: String },

    /// Resource already exists.
    #[error("{resource:?} already exists: {name}")]
    AlreadyExists { resource: Resource, name: String },

    // ─── Guard plugin rejection without a named resource ──────────────
    /// Operation precondition not met. Typically emitted by guard
    /// plugins detecting state drift / version mismatch / policy
    /// violations when the plugin cannot name a specific resource.
    /// Retrying without state change won't help — fix the precondition
    /// first.
    ///
    /// When a guard plugin **can** name the affected resource it should
    /// supply `resource_id` in `GuardDecision::Reject` so the failure
    /// routes through [`Self::NotFound`] instead with a real
    /// `resource_name`. See `oagw::domain::plugin::GuardDecision` docs.
    #[error("precondition failed [{precondition_type}/{subject}]: {detail}")]
    FailedPrecondition {
        /// `type_` from the canonical `PreconditionViolation`. OAGW
        /// currently emits `"STATE"` for guard rejections; other values
        /// may appear if the impl grows new precondition categories.
        precondition_type: String,
        /// `subject` from the canonical `PreconditionViolation` — the
        /// guard plugin's `error_code`.
        subject: String,
        detail: String,
    },

    /// Concurrency conflict. Typically emitted by guard plugins
    /// detecting concurrent edits when the plugin cannot name the
    /// conflicting resource. Often retryable with backoff (unlike
    /// [`Self::AlreadyExists`], which signals a resource collision that
    /// won't clear by retrying).
    ///
    /// When the guard **can** name the conflicting resource it should
    /// supply `resource_id` so the failure routes through
    /// [`Self::AlreadyExists`] instead.
    #[error("aborted [{reason}]: {detail}")]
    Aborted {
        /// Plugin-supplied error code from the canonical
        /// `Aborted.ctx.reason`.
        reason: String,
        detail: String,
    },

    // ─── Internal / catch-all ─────────────────────────────────────────
    /// Internal gateway error. Not retryable; surface to operator.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Catch-all for canonical categories the SDK does not specifically
    /// model — `Cancelled`, `Unknown`, `Unimplemented`, `DataLoss`, plus
    /// any new canonical category added after this SDK version. Preserves
    /// the full [`CanonicalError`] so consumers can `match` on the inner
    /// variant for typed dispatch on the unmodeled categories.
    ///
    /// All canonical categories OAGW emits in production today are
    /// modeled by dedicated variants above; reaching `Other` indicates
    /// either a future category or a category the impl doesn't normally
    /// emit (e.g. a guard plugin returning `Cancelled`).
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

// ─────────────────────────────────────────────────────────────────────
// CanonicalError → ServiceGatewayError projection
//
// Each sub-enum (`AuthFailureReason`, `PermissionDenialReason`,
// `TargetHostCode`, `Resource`) lives next to its wire-string constants
// in `crate::reason::auth`, `crate::reason::permission`, `crate::field`,
// and `crate::gts` respectively. This file owns only the top-level
// `ServiceGatewayError` enum and the dispatch logic that maps from
// `CanonicalError`.
// ─────────────────────────────────────────────────────────────────────

impl From<CanonicalError> for ServiceGatewayError {
    fn from(err: CanonicalError) -> Self {
        let detail = err.detail().to_owned();
        let resource_name = err.resource_name().unwrap_or("").to_owned();
        let resource = Resource::from_wire(err.resource_type().unwrap_or(""));

        match &err {
            // OAGW only emits ResourceExhausted with subject=quota::RATE_LIMIT
            // today. If the impl ever emits a different subject, the
            // projection will silently misrepresent it — revisit then.
            CanonicalError::ResourceExhausted { ctx, .. } => Self::RateLimited {
                retry_after_secs: ctx.violations.first().and_then(|v| v.retry_after_seconds),
            },

            CanonicalError::DeadlineExceeded { .. } => Self::Timeout,

            CanonicalError::ServiceUnavailable { ctx, .. } => Self::Unavailable {
                retry_after_secs: ctx.retry_after_seconds,
            },

            CanonicalError::Unauthenticated { ctx, .. } => Self::AuthFailed {
                reason: AuthFailureReason::from_wire(ctx.reason.as_deref()),
                detail,
            },

            CanonicalError::PermissionDenied { ctx, .. } => Self::PermissionDenied {
                reason: PermissionDenialReason::from_wire(ctx.reason.as_str()),
                detail,
            },

            // OAGW emits OutOfRange only for PayloadTooLarge and guard
            // 413; both wrap a single field_violation. Collapse to the
            // semantic name consumers care about.
            CanonicalError::OutOfRange { .. } => Self::PayloadTooLarge { detail },

            CanonicalError::InvalidArgument { ctx, .. } => project_invalid_argument(ctx, detail),

            CanonicalError::NotFound { .. } => Self::NotFound {
                resource,
                name: resource_name,
            },

            CanonicalError::AlreadyExists { .. } => Self::AlreadyExists {
                resource,
                name: resource_name,
            },

            // Guard plugin rejection at status 404 without `resource_id`:
            // the impl maps it to canonical FailedPrecondition with a
            // single PreconditionViolation carrying the plugin's
            // (error_code, detail, "STATE"). Take the first violation;
            // OAGW only ever emits one.
            CanonicalError::FailedPrecondition { ctx, .. } => {
                if let Some(v) = ctx.violations.first() {
                    Self::FailedPrecondition {
                        precondition_type: v.type_.clone(),
                        subject: v.subject.clone(),
                        detail: v.description.clone(),
                    }
                } else {
                    // Defensive — canonical FailedPrecondition with no
                    // violations; surface the canonical detail.
                    Self::FailedPrecondition {
                        precondition_type: String::new(),
                        subject: String::new(),
                        detail,
                    }
                }
            }

            // Guard plugin rejection at status 409 without `resource_id`:
            // canonical Aborted carries the plugin's error_code in
            // ctx.reason and the plugin's detail at the top level.
            CanonicalError::Aborted { ctx, .. } => Self::Aborted {
                reason: ctx.reason.clone(),
                detail,
            },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            _ => Self::Other {
                canonical: err.clone(),
            },
        }
    }
}

fn project_invalid_argument(ctx: &InvalidArgument, detail: String) -> ServiceGatewayError {
    let first_violation = match ctx {
        InvalidArgument::FieldViolations { field_violations } => field_violations.first(),
        _ => None,
    };

    let Some(v) = first_violation else {
        // Format / Constraint variant, or empty FieldViolations.
        return ServiceGatewayError::Validation {
            field: String::new(),
            reason: String::new(),
            detail,
        };
    };

    // Target-host reasons get their own typed variant.
    if let Some(code) = TargetHostCode::from_wire(v.reason.as_str()) {
        return ServiceGatewayError::InvalidTargetHost {
            code,
            detail: v.description.clone(),
        };
    }

    // Body-size reason collapses to PayloadTooLarge alongside the
    // canonical `OutOfRange` arm above.
    if v.reason == crate::field::PAYLOAD_TOO_LARGE {
        return ServiceGatewayError::PayloadTooLarge {
            detail: v.description.clone(),
        };
    }

    ServiceGatewayError::Validation {
        field: v.field.clone(),
        reason: v.reason.clone(),
        detail: v.description.clone(),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Streaming error — not a canonical category, retained as-is
// ─────────────────────────────────────────────────────────────────────

/// Errors produced by the streaming helpers in this SDK.
///
/// `StreamingError` is **not** a canonical error — it surfaces failures
/// inside the SDK's own SSE/WebSocket decoders that never cross a wire
/// boundary as an OAGW response.
#[derive(Debug, thiserror::Error)]
pub enum StreamingError {
    /// SSE parse error — a chunk could not be decoded as UTF-8.
    #[error("SSE parse error: {detail}")]
    ServerEventsParse { detail: String },

    /// Underlying byte stream produced an error.
    #[error("stream error: {0}")]
    Stream(#[from] Box<dyn std::error::Error + Send + Sync>),

    /// WebSocket connection to upstream failed.
    #[error("WebSocket connect error: {detail}")]
    WebSocketConnect { detail: String },

    /// WebSocket bridge error during forwarding.
    #[error("WebSocket bridge error: {detail}")]
    WebSocketBridge { detail: String },
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod sdk_vocabulary_round_trip_tests {
    //! Round-trip tests pinning the SDK vocabulary to the canonical wire.
    //!
    //! Each constant exposed by [`crate::reason`], [`crate::field`], and
    //! [`crate::quota`] MUST round-trip from the canonical builder into
    //! the [`Problem`] JSON body unchanged. The tests construct a
    //! `CanonicalError` using the SDK constant, render it to `Problem`,
    //! and assert the same string lands in the expected context field.
    use crate::{field, quota, reason};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    #[resource_error(gts_id!("cf.core.oagw.proxy.v1~"))]
    struct ProxyScope;

    fn problem(err: CanonicalError) -> serde_json::Value {
        let p = Problem::from(err);
        serde_json::to_value(&p).expect("Problem serializes")
    }

    #[test]
    fn auth_reason_constants_round_trip_to_unauthenticated_context() {
        for r in [
            reason::auth::PLUGIN_NOT_FOUND,
            reason::auth::PLUGIN_FAILED,
            reason::auth::PLUGIN_INTERNAL,
            reason::auth::AUTHORIZATION_REQUIRED,
        ] {
            let err = CanonicalError::unauthenticated().with_reason(r).create();
            let json = problem(err);
            assert_eq!(
                json["context"]["reason"], r,
                "auth reason {r} must reach unauthenticated.ctx.reason",
            );
        }
    }

    #[test]
    fn permission_reason_constants_round_trip_to_permission_denied_context() {
        for r in [
            reason::permission::AUTHZ_DENIED,
            reason::permission::TENANT_RESOLVER_UNAUTHORIZED,
            reason::permission::TENANT_BOUNDARY_VIOLATION,
            reason::permission::CORS_ORIGIN_NOT_ALLOWED,
            reason::permission::CORS_METHOD_NOT_ALLOWED,
        ] {
            let err = ProxyScope::permission_denied().with_reason(r).create();
            let json = problem(err);
            assert_eq!(
                json["context"]["reason"], r,
                "permission reason {r} must reach permission_denied.ctx.reason",
            );
        }
    }

    #[test]
    fn field_constants_round_trip_to_invalid_argument_violations() {
        let cases = [
            field::VALIDATION,
            field::REQUIRED,
            field::INVALID_PLUGIN_CONFIG,
            field::MISSING_TARGET_HOST,
            field::INVALID_TARGET_HOST,
            field::UNKNOWN_TARGET_HOST,
            field::PAYLOAD_TOO_LARGE,
            field::INVALID_MIME_TYPE,
            field::INVALID_VALUE,
            field::DUPLICATE_HEADER,
            field::INVALID_GTS_FORMAT,
            field::INVALID_GTS_UUID,
            field::INVALID_GTS_SCHEMA,
            field::MISSING_GTS_TILDE,
            field::INVALID_CORS_ORIGIN,
            field::CORS_CREDENTIALS_WITH_WILDCARD,
            field::WS_UPGRADE_REQUIRES_GET,
            field::WS_UPGRADE_BODY_FORBIDDEN,
            field::INVALID_PROXY_PATH,
            field::MISSING_ALIAS,
            field::INVALID_CONTENT_LENGTH,
            field::INVALID_REWRITTEN_URI,
            field::QUERY_NOT_ALLOWED,
            field::PATH_SUFFIX_NOT_ALLOWED,
            field::UNSUPPORTED_SCHEME,
            field::HTTP_UPSTREAM_FORBIDDEN,
        ];

        for code in cases {
            let err = ProxyScope::invalid_argument()
                .with_field_violation("test_field", "test description", code)
                .create();
            let json = problem(err);
            let violations = json["context"]["field_violations"]
                .as_array()
                .unwrap_or_else(|| panic!("field_violations must be an array for {code}"));
            let landed_reason = violations
                .iter()
                .find_map(|v| v.get("reason").and_then(|r| r.as_str()))
                .unwrap_or_else(|| panic!("field {code} produced no field_violations[].reason"));
            assert_eq!(
                landed_reason, code,
                "field code {code} must round-trip into field_violations[].reason",
            );
        }
    }

    #[test]
    fn quota_constants_round_trip_to_resource_exhausted_violations() {
        let err = ProxyScope::resource_exhausted("rate limit exceeded")
            .with_quota_violation(quota::RATE_LIMIT, "rate limit exceeded")
            .create();
        let json = problem(err);
        let violations = json["context"]["violations"]
            .as_array()
            .expect("violations must be an array");
        let subject = violations
            .iter()
            .find_map(|v| v.get("subject").and_then(|s| s.as_str()))
            .expect("quota violation must carry a subject");
        assert_eq!(
            subject,
            quota::RATE_LIMIT,
            "quota::RATE_LIMIT must round-trip into violations[].subject",
        );
    }
}

#[cfg(test)]
mod projection_tests {
    //! Tests for `From<CanonicalError> for ServiceGatewayError`.
    //!
    //! Each test constructs a `CanonicalError` the way the OAGW impl crate
    //! would (using `#[resource_error]` builders and the SDK vocabulary
    //! constants) and verifies the projection lands on the expected typed
    //! `ServiceGatewayError` variant.
    use super::*;
    use crate::{field, reason};
    use toolkit_canonical_errors::{CanonicalError, resource_error};
    use toolkit_gts::gts_id;

    #[resource_error(gts_id!("cf.core.oagw.proxy.v1~"))]
    struct ProxyScope;

    #[resource_error(gts_id!("cf.core.oagw.upstream.v1~"))]
    struct UpstreamScope;

    #[resource_error(gts_id!("cf.core.oagw.route.v1~"))]
    struct RouteScope;

    #[resource_error(gts_id!("cf.core.oagw.auth_plugin.v1~"))]
    struct AuthPluginScope;

    #[test]
    fn resource_exhausted_projects_to_rate_limited() {
        let canonical = ProxyScope::resource_exhausted("rate limit exceeded")
            .with_quota_violation(crate::quota::RATE_LIMIT, "rate limit exceeded")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::RateLimited {
                retry_after_secs: None,
            } => {}
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn resource_exhausted_preserves_quota_retry_after() {
        let canonical = ProxyScope::resource_exhausted("rate limit exceeded")
            .with_quota_violation(crate::quota::RATE_LIMIT, "rate limit exceeded")
            .with_quota_violation_retry_after_seconds(45)
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::RateLimited {
                retry_after_secs: Some(45),
            } => {}
            other => panic!("expected RateLimited with retry=45, got {other:?}"),
        }
    }

    #[test]
    fn resource_exhausted_retry_after_survives_problem_round_trip() {
        // Full out-of-process chain: canonical → Problem JSON → Problem →
        // CanonicalError → ServiceGatewayError. Pins the retry hint at every
        // hop so an HTTP consumer using oagw-sdk gets the same value as an
        // in-process ClientHub caller.
        let canonical = ProxyScope::resource_exhausted("rate limit exceeded")
            .with_quota_violation(crate::quota::RATE_LIMIT, "rate limit exceeded")
            .with_quota_violation_retry_after_seconds(15)
            .create();

        let problem = toolkit_canonical_errors::Problem::from(canonical);
        let bytes = serde_json::to_vec(&problem).expect("Problem serializes");
        let restored: toolkit_canonical_errors::Problem =
            serde_json::from_slice(&bytes).expect("Problem deserializes");
        let restored_canonical =
            CanonicalError::try_from(restored).expect("Problem reconstructs as CanonicalError");

        match ServiceGatewayError::from(restored_canonical) {
            ServiceGatewayError::RateLimited {
                retry_after_secs: Some(15),
            } => {}
            other => {
                panic!("expected RateLimited with retry=15 after Problem round-trip, got {other:?}")
            }
        }
    }

    #[test]
    fn deadline_exceeded_projects_to_timeout() {
        let canonical = ProxyScope::deadline_exceeded("upstream did not respond in time").create();
        assert!(matches!(
            ServiceGatewayError::from(canonical),
            ServiceGatewayError::Timeout
        ));
    }

    #[test]
    fn service_unavailable_preserves_retry_after() {
        let canonical = CanonicalError::service_unavailable()
            .with_retry_after_seconds(30)
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Unavailable {
                retry_after_secs: Some(30),
            } => {}
            other => panic!("expected Unavailable with retry=30, got {other:?}"),
        }
    }

    #[test]
    fn unauthenticated_projects_auth_failure_reason() {
        let cases = [
            (
                reason::auth::PLUGIN_NOT_FOUND,
                AuthFailureReason::PluginNotFound,
            ),
            (reason::auth::PLUGIN_FAILED, AuthFailureReason::PluginFailed),
            (
                reason::auth::PLUGIN_INTERNAL,
                AuthFailureReason::PluginInternal,
            ),
            (
                reason::auth::AUTHORIZATION_REQUIRED,
                AuthFailureReason::AuthorizationRequired,
            ),
        ];
        for (wire_reason, expected) in cases {
            let canonical = CanonicalError::unauthenticated()
                .with_reason(wire_reason)
                .create();
            match ServiceGatewayError::from(canonical) {
                ServiceGatewayError::AuthFailed { reason, .. } => assert_eq!(reason, expected),
                other => panic!("expected AuthFailed, got {other:?}"),
            }
        }
    }

    #[test]
    fn unauthenticated_unknown_reason_preserved_in_catch_all() {
        let canonical = CanonicalError::unauthenticated()
            .with_reason("CUSTOM_PLUGIN_CODE")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::AuthFailed {
                reason: AuthFailureReason::Unknown(s),
                ..
            } => assert_eq!(s, "CUSTOM_PLUGIN_CODE"),
            other => panic!("expected AuthFailed::Unknown, got {other:?}"),
        }
    }

    #[test]
    fn permission_denied_projects_typed_reasons() {
        let cases = [
            (
                reason::permission::AUTHZ_DENIED,
                PermissionDenialReason::AuthzDenied,
            ),
            (
                reason::permission::CORS_ORIGIN_NOT_ALLOWED,
                PermissionDenialReason::CorsOriginNotAllowed,
            ),
            (
                reason::permission::CORS_METHOD_NOT_ALLOWED,
                PermissionDenialReason::CorsMethodNotAllowed,
            ),
            (
                reason::permission::TENANT_BOUNDARY_VIOLATION,
                PermissionDenialReason::TenantBoundaryViolation,
            ),
        ];
        for (wire_reason, expected) in cases {
            let canonical = ProxyScope::permission_denied()
                .with_reason(wire_reason)
                .create();
            match ServiceGatewayError::from(canonical) {
                ServiceGatewayError::PermissionDenied { reason, .. } => {
                    assert_eq!(reason, expected);
                }
                other => panic!("expected PermissionDenied for {wire_reason}, got {other:?}"),
            }
        }
    }

    #[test]
    fn out_of_range_projects_to_payload_too_large() {
        let canonical = ProxyScope::out_of_range("body too big")
            .with_field_violation("body", "exceeds 100MB limit", field::PAYLOAD_TOO_LARGE)
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::PayloadTooLarge { .. } => {}
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_with_target_host_codes_projects_to_invalid_target_host() {
        let cases = [
            (field::MISSING_TARGET_HOST, TargetHostCode::Missing),
            (field::INVALID_TARGET_HOST, TargetHostCode::Invalid),
            (field::UNKNOWN_TARGET_HOST, TargetHostCode::Unknown),
        ];
        for (wire_reason, expected) in cases {
            let canonical = ProxyScope::invalid_argument()
                .with_field_violation("x-target-host", "header issue", wire_reason)
                .create();
            match ServiceGatewayError::from(canonical) {
                ServiceGatewayError::InvalidTargetHost { code, .. } => assert_eq!(code, expected),
                other => panic!("expected InvalidTargetHost for {wire_reason}, got {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_argument_with_payload_too_large_field_projects_to_payload_too_large() {
        // The body-size validator emits InvalidArgument + PAYLOAD_TOO_LARGE
        // (separate from the OutOfRange path used by guard 413). Both
        // collapse to the same SDK variant.
        let canonical = ProxyScope::invalid_argument()
            .with_field_violation("body", "exceeds 100MB limit", field::PAYLOAD_TOO_LARGE)
            .create();
        assert!(matches!(
            ServiceGatewayError::from(canonical),
            ServiceGatewayError::PayloadTooLarge { .. }
        ));
    }

    #[test]
    fn invalid_argument_with_unmodeled_field_falls_through_to_validation() {
        let canonical = ProxyScope::invalid_argument()
            .with_field_violation("gts_id", "bad uuid", field::INVALID_GTS_UUID)
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Validation {
                field: f,
                reason: r,
                detail,
            } => {
                assert_eq!(f, "gts_id");
                assert_eq!(r, field::INVALID_GTS_UUID);
                assert_eq!(detail, "bad uuid");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_format_variant_projects_to_validation_with_empty_field() {
        // Sites that don't know which field failed use the Format variant.
        let canonical = ProxyScope::invalid_argument()
            .with_format("alias must be 1-63 chars")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Validation {
                field: f,
                reason: r,
                ..
            } => {
                assert!(f.is_empty());
                assert!(r.is_empty());
            }
            other => panic!("expected Validation with empty field, got {other:?}"),
        }
    }

    #[test]
    fn not_found_projects_resource_kind_from_scope() {
        let canonical = UpstreamScope::not_found("upstream gone")
            .with_resource("00000000-0000-0000-0000-000000000000")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::NotFound { resource, name } => {
                assert_eq!(resource, Resource::Upstream);
                assert_eq!(name, "00000000-0000-0000-0000-000000000000");
            }
            other => panic!("expected NotFound::Upstream, got {other:?}"),
        }

        let canonical = AuthPluginScope::not_found("plugin gone")
            .with_resource(gts_id!(
                "cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
            ))
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::NotFound { resource, .. } => {
                assert_eq!(resource, Resource::AuthPlugin);
            }
            other => panic!("expected NotFound::AuthPlugin, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_projects_resource_kind_from_scope() {
        let canonical = RouteScope::already_exists("route conflict")
            .with_resource("conflicting-alias")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::AlreadyExists { resource, name } => {
                assert_eq!(resource, Resource::Route);
                assert_eq!(name, "conflicting-alias");
            }
            other => panic!("expected AlreadyExists::Route, got {other:?}"),
        }
    }

    #[test]
    fn unknown_resource_scope_preserves_wire_string() {
        // If the impl ever emits a NotFound under a resource scope the
        // SDK doesn't model, the projection preserves the raw string.
        #[resource_error(gts_id!("cf.future.oagw.something_new.v1~"))]
        struct FutureScope;

        let canonical = FutureScope::not_found("???")
            .with_resource("future-id")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::NotFound {
                resource: Resource::Unknown(s),
                ..
            } => assert_eq!(s, gts_id!("cf.future.oagw.something_new.v1~")),
            other => panic!("expected NotFound::Unknown, got {other:?}"),
        }
    }

    #[test]
    fn internal_projects_to_internal() {
        let canonical = CanonicalError::internal("something broke").create();
        assert!(matches!(
            ServiceGatewayError::from(canonical),
            ServiceGatewayError::Internal { .. }
        ));
    }

    #[test]
    fn failed_precondition_projects_to_typed_variant() {
        // OAGW emits FailedPrecondition for guard 404 rejections without
        // a resource_id. The plugin's error_code becomes `subject`, the
        // plugin's detail becomes the violation `description` (and thus
        // the SDK variant's `detail`), the `type_` is "STATE" today.
        let canonical = ProxyScope::failed_precondition()
            .with_precondition_violation("STATE_DRIFT", "account state is stale", "STATE")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::FailedPrecondition {
                precondition_type,
                subject,
                detail,
            } => {
                assert_eq!(precondition_type, "STATE");
                assert_eq!(subject, "STATE_DRIFT");
                assert_eq!(detail, "account state is stale");
            }
            other => panic!("expected FailedPrecondition, got {other:?}"),
        }
    }

    #[test]
    fn aborted_projects_to_typed_variant() {
        // OAGW emits Aborted for guard 409 rejections without a
        // resource_id. The plugin's error_code becomes the `reason`,
        // the plugin's detail stays at the top level.
        let canonical = ProxyScope::aborted("concurrent edit detected")
            .with_reason("CONCURRENT_MUTATION")
            .create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Aborted { reason, detail } => {
                assert_eq!(reason, "CONCURRENT_MUTATION");
                assert_eq!(detail, "concurrent edit detected");
            }
            other => panic!("expected Aborted, got {other:?}"),
        }
    }

    #[test]
    fn unmodeled_canonical_category_falls_through_to_other() {
        // Cancelled is not emitted by OAGW today; the SDK doesn't model
        // it explicitly. Verify it lands in Other with the canonical
        // preserved verbatim so consumers can dispatch on the inner
        // variant.
        let canonical = ProxyScope::cancelled().create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Other {
                canonical: CanonicalError::Cancelled { .. },
            } => {}
            other => panic!("expected Other::Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn other_preserves_canonical_for_future_categories() {
        // Same as above but verifies Other.canonical exposes the wire
        // gts_type and detail for logging.
        let canonical = ProxyScope::unimplemented("not yet supported").create();
        match ServiceGatewayError::from(canonical) {
            ServiceGatewayError::Other { canonical: inner } => {
                assert!(inner.gts_type().contains("unimplemented"));
                assert_eq!(inner.detail(), "not yet supported");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
