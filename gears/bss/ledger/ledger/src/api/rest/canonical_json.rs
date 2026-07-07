//! `CanonicalJson<T>` — drop-in wrapper around `axum::Json<T>` that
//! converts `JsonRejection` into a canonical `Problem+json` 400.
//!
//! Without this, axum's default `JsonRejection` serialises as
//! `text/plain; charset=utf-8`, which the canonical-error middleware at
//! `toolkit::api::canonical_error_middleware` does not enrich — it gates on
//! `Content-Type: application/problem+json` and passes plain-text responses
//! through verbatim. The result is a 400 without `trace_id` / `instance`,
//! violating the RFC 9457 contract every other 4xx in this module honours and
//! contradicting the `.error_400(openapi)` declaration on each affected
//! handler.
//!
//! Pattern mirrors `toolkit::api::odata::OData`, which converts its own
//! rejections into `CanonicalError` the same way.

use axum::Json;
use axum::extract::{FromRequest, Request, rejection::JsonRejection};
use serde::de::DeserializeOwned;
use toolkit::api::canonical_prelude::CanonicalError;

/// Drop-in replacement for `axum::Json<T>` in handler extractor position.
/// Identical behaviour on success; on rejection produces a canonical
/// Problem-JSON 400 with `field=body` and a reason code derived from the
/// underlying [`JsonRejection`] variant.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalJson<T>(pub T);

impl<T, S> FromRequest<S> for CanonicalJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(body)) => Ok(CanonicalJson(body)),
            Err(rejection) => Err(json_rejection_to_canonical(&rejection)),
        }
    }
}

/// Map an axum `JsonRejection` to the canonical Problem-JSON 400.
/// The reason `code` is a `snake_case` machine identifier the caller can
/// branch on; the `message` is human-readable and may embed the underlying
/// axum diagnostic.
fn json_rejection_to_canonical(rej: &JsonRejection) -> CanonicalError {
    let (code, message) = classify_json_rejection(rej);
    crate::api::rest::error::json_rejection_canonical(code, message)
}

/// Classify a `JsonRejection` into a `(code, message)` pair. `code` matches
/// one of the well-known `snake_case` reasons documented for body failures so
/// clients can dispatch without parsing the human message.
fn classify_json_rejection(rej: &JsonRejection) -> (&'static str, String) {
    match rej {
        JsonRejection::JsonSyntaxError(_) => (
            "json_syntax_error",
            format!("request body is not valid JSON: {rej}"),
        ),
        JsonRejection::JsonDataError(_) => (
            "invalid_json_body",
            format!("request body could not be deserialized: {rej}"),
        ),
        JsonRejection::MissingJsonContentType(_) => (
            "missing_json_content_type",
            "expected request to have `Content-Type: application/json`".to_owned(),
        ),
        JsonRejection::BytesRejection(_) => (
            "json_body_read_error",
            format!("failed to read request body: {rej}"),
        ),
        // `JsonRejection` is `#[non_exhaustive]`; any future variant falls
        // through to the generic invalid-body bucket so clients still get a
        // Problem envelope.
        _ => ("invalid_json_body", format!("invalid request body: {rej}")),
    }
}
