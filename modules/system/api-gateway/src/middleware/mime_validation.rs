//! MIME type validation middleware for enforcing per-operation allowed Content-Type headers
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use http::Method;
use std::sync::Arc;

use modkit::api::{OperationSpec, Problem};

use crate::middleware::common;

/// Map from (method, path) to allowed content types
pub type MimeValidationMap = Arc<DashMap<(Method, String), Vec<&'static str>>>;

/// Build MIME validation map from operation specs
#[must_use]
pub fn build_mime_validation_map(specs: &[OperationSpec]) -> MimeValidationMap {
    let map = DashMap::new();

    for spec in specs {
        if let Some(ref allowed) = spec.allowed_request_content_types {
            let key = (spec.method.clone(), spec.path.clone());

            map.insert(key, allowed.clone());
        }
    }

    Arc::new(map)
}

/// Extract and normalize the Content-Type header value.
///
/// Strips parameters like charset from "application/json; charset=utf-8"
/// to just "application/json".
fn extract_content_type(req: &Request) -> Option<String> {
    let ct_header = req.headers().get(http::header::CONTENT_TYPE)?;
    let ct_str = ct_header.to_str().ok()?;
    let ct_main = ct_str.split(';').next().map_or(ct_str, str::trim);
    Some(ct_main.to_owned())
}

/// Create an Unsupported Media Type error response.
fn create_unsupported_media_type_error(detail: String) -> Response {
    Problem::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "Unsupported Media Type",
        detail,
    )
    .into_response()
}

/// Validate that the content type is in the allowed list.
///
/// Returns Ok(()) if allowed, Err(Response) with error details if not.
fn validate_content_type(
    content_type: &str,
    allowed_types: &[&str],
    method: &Method,
    path: &str,
) -> Result<(), Box<Response>> {
    if allowed_types.contains(&content_type) {
        return Ok(());
    }

    tracing::warn!(
        method = %method,
        path = %path,
        content_type = content_type,
        allowed_types = ?allowed_types,
        "MIME type not allowed for this endpoint"
    );

    let detail = format!(
        "Content-Type '{}' is not allowed for this endpoint. Allowed types: {}",
        content_type,
        allowed_types.join(", ")
    );

    Err(Box::new(create_unsupported_media_type_error(detail)))
}

/// MIME validation middleware
///
/// Checks the Content-Type header against the allowed types configured
/// for the operation. Returns 415 Unsupported Media Type if the content
/// type is not allowed.
pub async fn mime_validation_middleware(
    validation_map: MimeValidationMap,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    // Use MatchedPath extension (set by Axum router) for accurate route matching
    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned());

    let path = common::resolve_path(&req, path.as_str());

    // Check if this operation has MIME validation configured
    let Some(allowed_types) = validation_map.get(&(method.clone(), path.clone())) else {
        // No validation configured - proceed
        return next.run(req).await;
    };

    // Extract and validate Content-Type header
    let Some(content_type) = extract_content_type(&req) else {
        tracing::warn!(
            method = %method,
            path = %path,
            allowed_types = ?allowed_types.value(),
            "Missing Content-Type header for endpoint with MIME validation"
        );

        let detail = format!(
            "Missing Content-Type header. Allowed types: {}",
            allowed_types.join(", ")
        );
        return create_unsupported_media_type_error(detail);
    };

    // Validate the content type
    if let Err(error_response) =
        validate_content_type(&content_type, &allowed_types, &method, &path)
    {
        return *error_response;
    }

    // Validation passed - proceed
    next.run(req).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mime_validation_tests.rs"]
mod mime_validation_tests;
