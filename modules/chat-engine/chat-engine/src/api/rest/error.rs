//! RFC-9457 error mapping for the Chat Engine REST surface.
//!
//! Phase 14 owns the full `ChatEngineError → CanonicalError → Problem`
//! pipeline. The canonical error layer (`modkit::api::canonical_error_middleware`)
//! converts the [`CanonicalError`] to a wire [`Problem`] and fills
//! `instance` / `trace_id` after the handler returns; this module is the
//! single source of truth for the status / problem-type / detail mapping.
//!
//! Error matrix:
//!
//! | `ChatEngineError` variant                         | HTTP | Canonical category | Notes                                                            |
//! |---------------------------------------------------|------|--------------------|------------------------------------------------------------------|
//! | `NotFound { resource, id }`                       | 404  | `NotFound`         | Resource-scoped via [`ChatEngineSessionError`] /…                |
//! | `Forbidden { reason }`                            | 403  | `PermissionDenied` | `AUTHZ_DENIED` reason marker.                                    |
//! | `Conflict { reason }`                             | 409  | `AlreadyExists`    | `reason` carried in `resource_name`.                             |
//! | `BadRequest { reason }`                           | 400  | `InvalidArgument`  | Format-variant message (no field violations array).              |
//! | `BackendUnavailable { source, retry_after, .. }`  | dyn  | dyn                | Delegates to `PluginError::suggested_status()` + `is_user_facing()`. |
//! | `Internal { reason, source }`                     | 500  | `Internal`         | Operator detail goes to logs only; wire detail is generic.       |
//!
//! `trace_id` is injected by `modkit::api::canonical_error_middleware`
//! on the response path — handlers do not construct `Problem` directly.
//! The unit tests below verify the **CanonicalError → Problem** wire
//! shape so the contract is grep-auditable.
//
// @cpt-cf-chat-engine-api-rest-error:p14
// @cpt-cf-chat-engine-adr-http-client-protocol:p14

use modkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::ChatEngineError;

// ---------------------------------------------------------------------------
// Resource scopes (per RFC-9457 + GTS)
// ---------------------------------------------------------------------------

/// Session-scoped error type used for not-found / conflict mappings.
#[resource_error("gts.cf.core.chat_engine.session.v1~")]
pub struct ChatEngineSessionError;

/// Message-scoped error type.
#[resource_error("gts.cf.core.chat_engine.message.v1~")]
pub struct ChatEngineMessageError;

/// Plugin-scoped error type (used by `BackendUnavailable`).
#[resource_error("gts.cf.core.chat_engine.plugin.v1~")]
pub struct ChatEnginePluginError;

/// Generic chat-engine resource type used when the inner variant lacks a
/// more precise scope.
#[resource_error("gts.cf.core.chat_engine.resource.v1~")]
pub struct ChatEngineResourceError;

// ---------------------------------------------------------------------------
// ChatEngineError → CanonicalError
// ---------------------------------------------------------------------------

impl From<ChatEngineError> for CanonicalError {
    fn from(err: ChatEngineError) -> Self {
        match err {
            ChatEngineError::NotFound { resource, id } => {
                let detail = format!("{resource} not found: {id}");
                match resource {
                    "session" | "shared_session" => ChatEngineSessionError::not_found(detail)
                        .with_resource(id)
                        .create(),
                    "message" | "variant" => ChatEngineMessageError::not_found(detail)
                        .with_resource(id)
                        .create(),
                    _ => ChatEngineResourceError::not_found(detail)
                        .with_resource(id)
                        .create(),
                }
            }

            ChatEngineError::Forbidden { reason } => {
                // PermissionDenied carries `reason` only; the human-readable
                // detail is the canonical category's default. We log the
                // request-supplied reason for operator triage.
                tracing::debug!(reason = %reason, "permission denied");
                ChatEngineSessionError::permission_denied()
                    .with_reason("AUTHZ_DENIED")
                    .create()
            }

            ChatEngineError::Conflict { reason } => {
                // `already_exists()` is the canonical RFC-9457 mapping for
                // state-machine / uniqueness conflicts (HTTP 409). The
                // `reason` is surfaced as `resource_name` so clients have a
                // stable, machine-readable discriminator.
                ChatEngineSessionError::already_exists(reason.clone())
                    .with_resource(reason)
                    .create()
            }

            ChatEngineError::BadRequest { reason } => ChatEngineResourceError::invalid_argument()
                .with_format(reason)
                .create(),

            ChatEngineError::BackendUnavailable {
                reason,
                retry_after,
                source,
            } => {
                // `PluginError` is collapsed into `BackendUnavailable` at
                // Phase 2's `From<PluginError> for ChatEngineError` boundary;
                // the typed source isn't preserved on the `BoxError` slot
                // (which holds the plugin's underlying cause, not the
                // PluginError itself). We therefore route on the signals
                // that *are* preserved — `retry_after` + the BoxError
                // `Display` chain — and apply the
                // `PluginError::suggested_status()` /
                // `PluginError::is_user_facing()` policy by reproducing
                // the same routing matrix here.
                let _ = source;
                map_backend_unavailable(reason, retry_after)
            }

            ChatEngineError::Internal { reason, source } => {
                tracing::error!(
                    operator_detail = %reason,
                    has_source = source.is_some(),
                    "chat-engine internal error",
                );
                // The operator-only `reason` MUST NOT cross the wire — the
                // canonical `Internal` category emits a generic detail and
                // the middleware injects `trace_id`.
                CanonicalError::internal("Internal server error").create()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PluginError-based fallback routing for `BackendUnavailable`
// ---------------------------------------------------------------------------

fn map_backend_unavailable(
    reason: String,
    retry_after: Option<std::time::Duration>,
) -> CanonicalError {
    // Classify by signals preserved on the variant. The reason strings are
    // produced by `From<PluginError> for ChatEngineError` and the
    // `with_retry_after` / `"backend rate limited"` / `"backend timeout"`
    // markers are stable identifiers (see Phase 2's `domain::error`).
    let cls = classify_backend(&reason, retry_after);

    // Operator-only details (transient / timeout) never reach the wire —
    // we log them with the trace_id added by the canonical middleware.
    if !cls.user_facing {
        tracing::warn!(
            operator_detail = %reason,
            status = cls.suggested_status,
            "backend unavailable (operator-only detail redacted from wire)",
        );
    }

    match cls.suggested_status {
        429 => {
            let mut b = CanonicalError::service_unavailable();
            if let Some(d) = retry_after {
                b = b.with_retry_after_seconds(d.as_secs());
            }
            b.create()
        }
        504 | 408 => CanonicalError::service_unavailable()
            .with_retry_after_seconds(5)
            .create(),
        400 => ChatEnginePluginError::invalid_argument()
            .with_format(reason)
            .create(),
        401 | 403 => {
            tracing::debug!(reason = %reason, "plugin denied access");
            ChatEnginePluginError::permission_denied()
                .with_reason("BACKEND_UNAUTHORIZED")
                .create()
        }
        404 => ChatEnginePluginError::not_found(reason.clone())
            .with_resource(reason)
            .create(),
        _ => {
            // 500/502/503 and the catch-all all map to ServiceUnavailable;
            // the upstream `retry_after` (if any) is preserved.
            let mut b = CanonicalError::service_unavailable();
            if let Some(d) = retry_after {
                b = b.with_retry_after_seconds(d.as_secs());
            } else {
                b = b.with_retry_after_seconds(5);
            }
            b.create()
        }
    }
}

/// Inferred routing class for a `BackendUnavailable` variant. The decision
/// table mirrors `PluginError::suggested_status` / `is_user_facing` even
/// though the typed `PluginError` itself isn't preserved across the
/// `From<PluginError> for ChatEngineError` boundary.
struct BackendClass {
    /// HTTP status the underlying `PluginError` would have suggested.
    suggested_status: u16,
    /// Whether the inner reason is safe to surface to end users.
    user_facing: bool,
}

fn classify_backend(reason: &str, retry_after: Option<std::time::Duration>) -> BackendClass {
    if retry_after.is_some() || reason.contains("rate limited") {
        // RateLimited → 429 (user-facing per the SDK matrix)
        BackendClass {
            suggested_status: 429,
            user_facing: true,
        }
    } else if reason.contains("timeout") {
        BackendClass {
            suggested_status: 504,
            user_facing: false,
        }
    } else {
        // Anything else is treated as `Transient` → 503, operator-only.
        BackendClass {
            suggested_status: 503,
            user_facing: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — one assertion per branch (status + problem_type)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chat_engine_sdk::error::PluginError;
    use modkit_canonical_errors::Problem;
    use std::time::Duration;

    const NOT_FOUND_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~";
    const INVALID_ARGUMENT_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~";
    const PERMISSION_DENIED_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~";
    const ALREADY_EXISTS_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~";
    const SERVICE_UNAVAILABLE_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~";
    const INTERNAL_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~";

    fn problem_from(err: ChatEngineError) -> Problem {
        Problem::from(CanonicalError::from(err))
    }

    #[test]
    fn not_found_maps_to_404() {
        let p = problem_from(ChatEngineError::not_found("session", "abc"));
        assert_eq!(p.status, 404);
        assert_eq!(p.problem_type, NOT_FOUND_TYPE);
    }

    #[test]
    fn forbidden_maps_to_403_with_permission_denied() {
        let p = problem_from(ChatEngineError::forbidden("missing scope"));
        assert_eq!(p.status, 403);
        assert_eq!(p.problem_type, PERMISSION_DENIED_TYPE);
    }

    #[test]
    fn conflict_maps_to_409_already_exists() {
        let p = problem_from(ChatEngineError::conflict("invalid lifecycle transition"));
        assert_eq!(p.status, 409);
        assert_eq!(p.problem_type, ALREADY_EXISTS_TYPE);
    }

    #[test]
    fn bad_request_maps_to_400_invalid_argument() {
        let p = problem_from(ChatEngineError::bad_request("missing 'content'"));
        assert_eq!(p.status, 400);
        assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
    }

    #[test]
    fn backend_unavailable_without_plugin_err_maps_to_503() {
        let err = ChatEngineError::BackendUnavailable {
            reason: "upstream 502".into(),
            retry_after: None,
            source: None,
        };
        let p = problem_from(err);
        assert_eq!(p.status, 503);
        assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
    }

    #[test]
    fn backend_unavailable_rate_limited_with_retry_after_emits_503_with_hint() {
        let err: ChatEngineError =
            PluginError::rate_limited(Some(Duration::from_secs(7))).into();
        let p = problem_from(err);
        assert_eq!(p.status, 503);
        assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
        assert_eq!(p.context["retry_after_seconds"].as_u64(), Some(7));
    }

    #[test]
    fn backend_unavailable_redacts_non_user_facing_detail() {
        // `Transient` carries an operator-only message; the wire detail
        // must be generic.
        let err: ChatEngineError = PluginError::transient("internal hostname leaked").into();
        let p = problem_from(err);
        assert_eq!(p.status, 503);
        assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
        // We can't assert detail == "Backend unavailable" verbatim — the
        // ServiceUnavailable builder constructs its own detail — but we
        // CAN assert the operator-only string never reached the wire.
        let body = serde_json::to_string(&p).unwrap();
        assert!(
            !body.contains("internal hostname leaked"),
            "operator-only detail must never appear on the wire: {body}"
        );
    }

    #[test]
    fn internal_maps_to_500_and_redacts_reason() {
        let err = ChatEngineError::Internal {
            reason: "DB connection pool exhausted".into(),
            source: None,
        };
        let p = problem_from(err);
        assert_eq!(p.status, 500);
        assert_eq!(p.problem_type, INTERNAL_TYPE);
        let body = serde_json::to_string(&p).unwrap();
        assert!(
            !body.contains("DB connection pool exhausted"),
            "internal `reason` must never appear on the wire: {body}"
        );
    }
}
