//! MIME type validation middleware for enforcing per-operation allowed
//! `Content-Type` headers.
//!
//! The [`MimeValidationMap`] (a `(method, path)` → allowed-types lookup) is
//! built by the consuming gear from its operation specs; this crate owns the
//! runtime type and the request-time middleware. Rejections are rendered under a
//! caller-supplied GTS `scope` so the consumer keeps ownership of its error
//! identity.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use http::Method;
use toolkit_canonical_errors::CanonicalError;

use crate::common;

type MimeKey = (Method, String);

/// Per-route allowed-`Content-Type` lookup, plus the GTS `scope` under which
/// rejections are rendered.
#[derive(Clone, Default)]
pub struct MimeValidationMap {
    allowed: Arc<DashMap<MimeKey, Vec<&'static str>>>,
    scope: &'static str,
}

impl MimeValidationMap {
    /// Build the map from `(method, path)` → allowed-content-type pairs, rendering
    /// rejections under `scope`.
    #[must_use]
    pub fn from_pairs(
        scope: &'static str,
        pairs: impl IntoIterator<Item = (MimeKey, Vec<&'static str>)>,
    ) -> Self {
        let allowed = DashMap::new();
        for (key, types) in pairs {
            allowed.insert(key, types);
        }
        Self {
            allowed: Arc::new(allowed),
            scope,
        }
    }

    /// Look up the allowed content types configured for `(method, path)`, if any.
    #[must_use]
    pub fn get(&self, method: &Method, path: &str) -> Option<Vec<&'static str>> {
        self.allowed
            .get(&(method.clone(), path.to_owned()))
            .map(|v| v.value().clone())
    }
}

/// Extract and normalize the `Content-Type` header value.
///
/// Strips parameters like charset from `application/json; charset=utf-8`
/// to just `application/json`.
fn extract_content_type(req: &Request) -> Option<String> {
    let ct_header = req.headers().get(http::header::CONTENT_TYPE)?;
    let ct_str = ct_header.to_str().ok()?;
    let ct_main = ct_str.split(';').next().map_or(ct_str, str::trim);
    Some(ct_main.to_owned())
}

/// Create a canonical `invalid_argument` Problem response for an unsupported
/// or missing Content-Type, under the caller-supplied GTS `scope`.
fn create_unsupported_media_type_error(
    scope: &'static str,
    detail: String,
    reason: &str,
) -> Response {
    let err = CanonicalError::scoped_invalid_argument(scope)
        .with_field_violation("content-type", detail, reason)
        .create();
    err.into_response()
}

/// Validate that the content type is in the allowed list.
///
/// Returns `Ok(())` if allowed, `Err(Response)` with error details if not.
fn validate_content_type(
    scope: &'static str,
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

    Err(Box::new(create_unsupported_media_type_error(
        scope,
        detail,
        "UNSUPPORTED_MEDIA_TYPE",
    )))
}

/// MIME validation middleware.
///
/// Checks the `Content-Type` header against the allowed types configured for the
/// operation. Returns 400 Bad Request with a canonical `invalid_argument`
/// Problem (`field_violations[0].reason` = `UNSUPPORTED_MEDIA_TYPE` or
/// `MISSING_CONTENT_TYPE`) if the content type is not allowed. Rejections are
/// rendered under `scope`.
pub async fn mime_validation_middleware(
    State(validation_map): State<MimeValidationMap>,
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
    let Some(allowed_types) = validation_map.get(&method, &path) else {
        // No validation configured - proceed
        return next.run(req).await;
    };

    let scope = validation_map.scope;

    // Extract and validate Content-Type header
    let Some(content_type) = extract_content_type(&req) else {
        tracing::warn!(
            method = %method,
            path = %path,
            allowed_types = ?allowed_types,
            "Missing Content-Type header for endpoint with MIME validation"
        );

        let detail = format!(
            "Missing Content-Type header. Allowed types: {}",
            allowed_types.join(", ")
        );
        return create_unsupported_media_type_error(scope, detail, "MISSING_CONTENT_TYPE");
    };

    // Validate the content type
    if let Err(error_response) =
        validate_content_type(scope, &content_type, &allowed_types, &method, &path)
    {
        return *error_response;
    }

    // Validation passed - proceed
    next.run(req).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #[test]
    fn test_content_type_parameter_stripping() {
        // Test the logic for stripping parameters from Content-Type
        let ct_with_charset = "application/json; charset=utf-8";
        let ct_main = ct_with_charset
            .split(';')
            .next()
            .map_or(ct_with_charset, str::trim);

        assert_eq!(ct_main, "application/json");

        // Test with multiple parameters
        let ct_complex = "multipart/form-data; boundary=----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let ct_main2 = ct_complex.split(';').next().map_or(ct_complex, str::trim);

        assert_eq!(ct_main2, "multipart/form-data");

        // Test without parameters
        let ct_simple = "application/pdf";
        let ct_main3 = ct_simple.split(';').next().map_or(ct_simple, str::trim);

        assert_eq!(ct_main3, "application/pdf");
    }
}
