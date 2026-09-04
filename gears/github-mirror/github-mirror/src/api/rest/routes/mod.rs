//! Route registration, split by API surface.
//!
//! [`github`] holds the GitHub-compatible endpoints (PRD §5.8) at the root,
//! mirroring GitHub's native paths (`/repos/{owner}/{name}/...`).
//! [`v1`] holds the gear's own extended endpoints under the versioned path
//! `/github-mirror/v1/` (PRD §5.9).

use std::sync::Arc;

use axum::body::Body;
use axum::http::header;
use axum::response::Response;
use axum::{Extension, Router};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

use crate::domain::service::Service;

pub mod github;
pub mod v1;

pub type ConcreteService = Service;

pub(crate) const API_TAG: &str = "GitHub Mirror";
pub(crate) const PAGE_DOC: &str = "Page number of the results to fetch (GitHub-style)";
pub(crate) const PER_PAGE_DOC: &str = "The number of results per page (max 100)";
pub(crate) const STATE_DOC: &str =
    "Filter by state: `open` (GitHub's default when omitted), `closed`, or `all`";

pub(crate) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Where GitHub points clients for error semantics; its own bodies carry the
/// same field.
const GITHUB_DOCS_URL: &str = "https://docs.github.com/rest";

/// Biggest error body worth rewriting. A problem document is a few hundred
/// bytes; anything larger is not one, and is dropped rather than rewritten.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// Restate a failed response in GitHub's error shape.
///
/// The platform answers errors as RFC-9457 `application/problem+json`, which
/// is right everywhere except here: PRD 5.8 promises a client can swap its
/// base URL for the mirror's, and such a client reads `message` out of an
/// `application/json` body (Octokit reads `documentation_url` too). Status
/// codes already match GitHub, so only the body is restated.
async fn github_error_body(response: Response) -> Response {
    let status = response.status();
    if !status.is_client_error() && !status.is_server_error() {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_ERROR_BODY).await {
        Ok(bytes) => bytes,
        Err(e) => {
            // The body is gone, so the announced length no longer describes
            // what is sent; leaving `Content-Length` would make the response
            // unparseable.
            tracing::warn!(
                %status,
                content_length = ?parts.headers.get(header::CONTENT_LENGTH),
                error = %e,
                "error body too large or unreadable; answering without it"
            );
            parts.headers.remove(header::CONTENT_LENGTH);
            return Response::from_parts(parts, Body::empty());
        }
    };

    // `title` is the human-readable summary ("Not Found"), which is what
    // GitHub puts in `message`; `detail` is the fallback when it is absent.
    let message = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|problem| {
            problem
                .get("title")
                .or_else(|| problem.get("detail"))
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Unknown error")
                .to_owned()
        });

    let Ok(rendered) = serde_json::to_vec(&serde_json::json!({
        "message": message,
        "documentation_url": GITHUB_DOCS_URL,
    })) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(rendered))
}

pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    router = v1::register_routes(router, openapi);

    // The GitHub-compatible routes are built separately so the error-body
    // rewrite lands on them alone: the gear's own `/github-mirror/v1` surface
    // keeps the platform's RFC-9457 bodies.
    let compat = github::register_routes(Router::new(), openapi)
        .layer(axum::middleware::map_response(github_error_body));
    router = router.merge(compat);

    router.layer(Extension(service))
}
