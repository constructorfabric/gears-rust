//! Conversion from [`TransportError`] into [`toolkit_canonical_errors::CanonicalError`].
//!
//! Lives in `toolkit-contract` (not in `toolkit-canonical-errors`) so the
//! canonical-errors crate stays a leaf in the workspace dep graph. Gated
//! behind the `canonical-errors` feature.
//!
//! # Mapping policy
//!
//! When the peer participates in the canonical-errors envelope (RFC 9457
//! `Problem` with a `gts://...` `type` URI, either inline on the HTTP
//! response body or attached as the `x-modkit-problem-bin` gRPC trailer),
//! the typed `CanonicalError::*` variant is recovered via
//! [`toolkit_canonical_errors::CanonicalError::try_from(Problem)`]. Resource
//! info (`resource_type`, `resource_name`) is pulled out of
//! `Problem.context` so callers can `matches!(err, CanonicalError::NotFound
//! { .. })` after the conversion.
//!
//! Fallbacks for peers that don't speak the envelope:
//! - [`TransportError::HttpStatus`]: resource-scoped statuses (404 / 409 /
//!   403) construct the matching variant with `resource_type = "unknown"`
//!   and `resource_name = "unknown"` via a synthetic `Problem`.
//! - [`TransportError::Grpc`]: resource-scoped codes (`NotFound`,
//!   `AlreadyExists`, `PermissionDenied`) likewise construct the matching
//!   variant with synthetic "unknown" resource info.
//! - Other categories (Internal, Unavailable, Unauthenticated, ...) map
//!   directly via the canonical category mapping.

use toolkit_canonical_errors::{CanonicalError, Problem, ProblemCategory};

use crate::runtime::transport_error::TransportError;

impl From<TransportError> for CanonicalError {
    fn from(err: TransportError) -> Self {
        match err {
            TransportError::Problem(problem) => problem_to_canonical(problem),
            TransportError::HttpStatus { status, body } => http_status_to_canonical(status, &body),
            #[cfg(feature = "grpc-client")]
            TransportError::Grpc { code, message } => grpc_code_to_canonical(code, message),
            TransportError::Network(_msg) => CanonicalError::service_unavailable().create(),
            TransportError::Timeout(d) => {
                CanonicalError::internal(format!("timeout after {d:?}")).create()
            }
            TransportError::Serialization(msg) => {
                CanonicalError::internal(format!("serialization error: {msg}")).create()
            }
            TransportError::Sse(msg) => {
                CanonicalError::internal(format!("SSE protocol error: {msg}")).create()
            }
            TransportError::UrlBuild(msg) => {
                CanonicalError::internal(format!("URL build error: {msg}")).create()
            }
        }
    }
}

fn problem_to_canonical(problem: Problem) -> CanonicalError {
    let status = problem.status;
    let title = problem.title.clone();
    let detail = problem.detail.clone();
    match CanonicalError::try_from(problem) {
        Ok(err) => err,
        Err(_) => http_status_to_canonical(status, &format!("{title}: {detail}")),
    }
}

fn synth_problem(category: ProblemCategory, detail: &str) -> Problem {
    Problem {
        problem_type: format!("gts://{}", category.gts_fragment()),
        title: category.title().to_owned(),
        status: category.http_status(),
        detail: detail.to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({
            "resource_type": "unknown",
            "resource_name": "unknown",
        }),
        error_code: None,
        error_domain: None,
    }
}

#[allow(
    clippy::expect_used,
    reason = "synth_problem unconditionally constructs problem_type from ProblemCategory::canonical_type(), which is the canonical GTS URI registry — CanonicalError::try_from cannot fail for any input synth_problem can produce."
)]
fn synth_to_canonical(category: ProblemCategory, detail: &str) -> CanonicalError {
    CanonicalError::try_from(synth_problem(category, detail))
        .expect("synthetic problem_type is always a known canonical GTS URI")
}

fn http_status_to_canonical(status: u16, body: &str) -> CanonicalError {
    let preview: &str = if body.len() > 200 { &body[..200] } else { body };
    match status {
        401 => CanonicalError::unauthenticated()
            .with_reason(preview.to_owned())
            .create(),
        403 => synth_to_canonical(ProblemCategory::PermissionDenied, preview),
        404 => synth_to_canonical(ProblemCategory::NotFound, preview),
        409 => synth_to_canonical(ProblemCategory::AlreadyExists, preview),
        503 => CanonicalError::service_unavailable().create(),
        s => CanonicalError::internal(format!("HTTP {s}: {preview}")).create(),
    }
}

#[cfg(feature = "grpc-client")]
fn grpc_code_to_canonical(code: tonic::Code, message: String) -> CanonicalError {
    use tonic::Code;
    match code {
        Code::Unauthenticated => CanonicalError::unauthenticated()
            .with_reason(message)
            .create(),
        Code::Unavailable => CanonicalError::service_unavailable().create(),
        Code::NotFound => synth_to_canonical(ProblemCategory::NotFound, &message),
        Code::AlreadyExists => synth_to_canonical(ProblemCategory::AlreadyExists, &message),
        Code::PermissionDenied => synth_to_canonical(ProblemCategory::PermissionDenied, &message),
        other => CanonicalError::internal(format!("gRPC {other:?}: {message}")).create(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn problem_not_found_preserves_category() {
        let original = toolkit_canonical_errors::Problem::from_error(
            &CanonicalError::try_from(synth_problem(ProblemCategory::NotFound, "missing")).unwrap(),
        )
        .unwrap();
        let err: CanonicalError = TransportError::Problem(original).into();
        assert!(matches!(err, CanonicalError::NotFound { .. }));
    }

    #[test]
    fn http_404_fallback_yields_not_found() {
        let err: CanonicalError = TransportError::HttpStatus {
            status: 404,
            body: "missing".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::NotFound { .. }));
    }

    #[test]
    fn http_403_fallback_yields_permission_denied() {
        let err: CanonicalError = TransportError::HttpStatus {
            status: 403,
            body: "nope".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::PermissionDenied { .. }));
    }

    #[test]
    fn http_409_fallback_yields_already_exists() {
        let err: CanonicalError = TransportError::HttpStatus {
            status: 409,
            body: "dup".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::AlreadyExists { .. }));
    }

    #[cfg(feature = "grpc-client")]
    #[test]
    fn grpc_not_found_preserves_category() {
        let err: CanonicalError = TransportError::Grpc {
            code: tonic::Code::NotFound,
            message: "missing".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::NotFound { .. }));
    }

    #[cfg(feature = "grpc-client")]
    #[test]
    fn grpc_already_exists_preserves_category() {
        let err: CanonicalError = TransportError::Grpc {
            code: tonic::Code::AlreadyExists,
            message: "dup".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::AlreadyExists { .. }));
    }

    #[cfg(feature = "grpc-client")]
    #[test]
    fn grpc_permission_denied_preserves_category() {
        let err: CanonicalError = TransportError::Grpc {
            code: tonic::Code::PermissionDenied,
            message: "nope".into(),
        }
        .into();
        assert!(matches!(err, CanonicalError::PermissionDenied { .. }));
    }
}
