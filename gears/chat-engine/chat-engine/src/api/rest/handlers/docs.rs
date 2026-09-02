//! Handlers for the gear-scoped API reference (`/chat-engine/v1/docs`) and
//! the document behind it (`/chat-engine/v1/openapi`).
//!
//! Both are anonymous: an API reference that demands a bearer token before it
//! will tell you which endpoints exist is not a reference.
//
// @cpt-cf-chat-engine-api-rest-docs

use std::sync::Arc;

use axum::Extension;
use axum::extract::OriginalUri;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

use crate::api::rest::docs::{DOCS_PAGE, GearOpenApiDoc};

/// `GET /chat-engine/v1/openapi` — the gear's OpenAPI 3.1 document.
///
/// [`OriginalUri`] rather than the nested `Uri`: the gateway mounts this router
/// under `prefix_path`, and the document's `servers` entry has to reflect the
/// path the client actually called.
pub async fn openapi_json(
    OriginalUri(uri): OriginalUri,
    Extension(doc): Extension<Arc<GearOpenApiDoc>>,
) -> Response {
    match doc.render(uri.path()) {
        Some(json) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            json,
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenAPI document unavailable",
        )
            .into_response(),
    }
}

/// `GET /chat-engine/v1/docs` — the API reference page rendering the document above.
pub async fn docs_page() -> Html<&'static str> {
    Html(DOCS_PAGE)
}

#[cfg(test)]
#[path = "docs_tests.rs"]
mod docs_tests;
