//! Centralized error mapping for Axum
//!
//! Converts framework and gear errors into `CanonicalError`. Wire rendering
//! (RFC 9457 `application/problem+json`), `instance`, and `trace_id` are
//! attached by `IntoResponse for CanonicalError` plus the
//! `canonical_error_middleware` (`crate::api::canonical_error_layer`).

use axum::{extract::Request, http::HeaderMap, middleware::Next, response::Response};
use std::any::Any;

use crate::config::ConfigError;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Error as ODataError;

/// Passthrough middleware kept for backwards compatibility with the api-gateway
/// layer stack. The real work (logging `diagnostic()`, filling `trace_id` /
/// `instance`) is now done by `canonical_error_middleware`.
pub async fn error_mapping_middleware(request: Request, next: Next) -> Response {
    next.run(request).await
}

/// Extract trace ID from headers or generate one
pub fn extract_trace_id(headers: &HeaderMap) -> Option<String> {
    // Try to get trace ID from various common headers
    headers
        .get("x-trace-id")
        .or_else(|| headers.get("x-request-id"))
        .or_else(|| headers.get("traceparent"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            // Try to get from current tracing span
            tracing::Span::current()
                .id()
                .map(|id| id.into_u64().to_string())
        })
}

/// Centralized downcast-based error mapping.
///
/// Converts known framework and gear error types into `CanonicalError`. The
/// descriptive detail for `Internal`-category mappings flows into the
/// canonical's `ctx.description` (recoverable via `diagnostic()`) so the
/// `canonical_error_middleware` can log it server-side without leaking onto
/// the wire (`detail` stays opaque per DESIGN.md §3.6).
#[must_use]
pub fn map_error_to_canonical(error: &dyn Any) -> CanonicalError {
    if let Some(odata_err) = error.downcast_ref::<ODataError>() {
        return CanonicalError::from(odata_err.clone());
    }

    if let Some(config_err) = error.downcast_ref::<ConfigError>() {
        let detail = match config_err {
            ConfigError::GearNotFound { gear } => {
                format!("Gear '{gear}' configuration not found")
            }
            ConfigError::InvalidGearStructure { gear } => {
                format!("Gear '{gear}' has invalid configuration structure")
            }
            ConfigError::MissingConfigSection { gear } => {
                format!("Gear '{gear}' is missing required config section")
            }
            ConfigError::InvalidConfig { gear, .. } => {
                format!("Gear '{gear}' has invalid configuration")
            }
            ConfigError::VarExpand { gear, cause } => {
                // The `cause` carries the failing env-var name. It is logged
                // locally for operators but intentionally NOT placed into the
                // canonical's diagnostic — `diagnostic()` is exposed through
                // `canonical_error_middleware` and we keep the env-var name
                // out of any path that could reach a downstream consumer.
                tracing::error!(
                    gear =  %gear,
                    error = %cause,
                    "Environment variable expansion failed in gear config"
                );
                format!("Gear '{gear}' has invalid environment-backed configuration")
            }
        };
        return CanonicalError::internal(detail).create();
    }

    if let Some(anyhow_err) = error.downcast_ref::<anyhow::Error>() {
        return CanonicalError::internal(format!("{anyhow_err:#}")).create();
    }

    CanonicalError::internal("unknown error type in error mapping layer").create()
}

/// Helper trait for converting concrete error types into `CanonicalError`.
///
/// Prefer `impl From<E> for CanonicalError` for new error types — this trait
/// exists for cases where the conversion is performed through a dynamic
/// downcast (see [`map_error_to_canonical`]).
pub trait IntoCanonical {
    fn into_canonical(self) -> CanonicalError;
}

impl IntoCanonical for ODataError {
    fn into_canonical(self) -> CanonicalError {
        CanonicalError::from(self)
    }
}

impl IntoCanonical for ConfigError {
    fn into_canonical(self) -> CanonicalError {
        map_error_to_canonical(&self as &dyn Any)
    }
}

impl IntoCanonical for anyhow::Error {
    fn into_canonical(self) -> CanonicalError {
        map_error_to_canonical(&self as &dyn Any)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_canonical_errors::Problem;

    #[test]
    fn odata_error_maps_to_invalid_argument() {
        let canonical = ODataError::InvalidFilter("malformed".to_owned()).into_canonical();
        assert_eq!(canonical.status_code(), 400);
        assert!(canonical.gts_type().contains("invalid_argument"));

        // Wire serialization shape — instance / trace_id are filled by middleware.
        let problem = Problem::from(canonical);
        assert_eq!(problem.status, Some(400));
        assert!(problem.instance.is_none());
    }

    #[test]
    fn config_gear_not_found_preserves_diagnostic() {
        let canonical = ConfigError::GearNotFound {
            gear: "test_gear".to_owned(),
        }
        .into_canonical();

        assert_eq!(canonical.status_code(), 500);
        assert!(canonical.gts_type().contains("internal"));

        // Gear name reaches `diagnostic()` (logged by middleware) but never
        // the wire `detail` (which stays the canonical opaque string).
        let diag = canonical.diagnostic().expect("Internal carries diagnostic");
        assert!(diag.contains("test_gear"), "diagnostic was {diag:?}");

        let problem = Problem::from(canonical);
        assert!(
            !problem.detail.contains("test_gear"),
            "wire detail leaked gear name: {}",
            problem.detail
        );
    }

    #[test]
    fn anyhow_error_preserves_diagnostic() {
        let canonical = anyhow::anyhow!("Something went wrong").into_canonical();
        assert_eq!(canonical.status_code(), 500);
        let diag = canonical.diagnostic().expect("Internal carries diagnostic");
        assert!(diag.contains("Something went wrong"));
    }

    #[test]
    fn config_var_expand_redacts_env_and_source_from_diagnostic() {
        let source = toolkit_utils::var_expand::ExpandVarsError::Var {
            name: "SECRET_API_KEY".to_owned(),
            source: std::env::VarError::NotPresent,
        };
        let canonical = ConfigError::VarExpand {
            gear: "my_mod".to_owned(),
            cause: source,
        }
        .into_canonical();

        assert_eq!(canonical.status_code(), 500);

        // The env-var name and the source error message MUST NOT reach either
        // the wire or the diagnostic — only the gear name is allowed
        // through, and only via `diagnostic()`.
        let diag = canonical.diagnostic().expect("Internal carries diagnostic");
        assert!(
            !diag.contains("SECRET_API_KEY"),
            "diagnostic leaked env var name: {diag}"
        );
        assert!(
            !diag.contains("not present"),
            "diagnostic leaked source text: {diag}"
        );
        assert!(
            diag.contains("my_mod"),
            "diagnostic dropped gear name: {diag}"
        );

        let problem = Problem::from(canonical);
        assert!(!problem.detail.contains("SECRET_API_KEY"));
        assert!(!problem.detail.contains("not present"));
        assert!(!problem.detail.contains("my_mod"));
    }

    #[test]
    fn test_extract_trace_id_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-trace-id", "test-trace-123".parse().unwrap());

        let trace_id = extract_trace_id(&headers);
        assert_eq!(trace_id, Some("test-trace-123".to_owned()));
    }
}
