//! `Query<T>` - a drop-in `axum::extract::Query<T>` replacement whose
//! extraction failures render as `application/problem+json` (RFC 9457
//! `Problem`) instead of axum's default plain-text rejection body. Same
//! class of gap as `extract::Json`, same fix shape - see this module's
//! parent doc comment.

use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Query as AxumQuery};
use axum::http::request::Parts;
use toolkit_canonical_errors::CanonicalError;

use super::error::rejection_to_canonical;

/// Drop-in replacement for `axum::extract::Query<T>` as a handler parameter.
/// Extraction success is identical to `axum::extract::Query<T>`; extraction
/// failure produces a `CanonicalError` instead of `QueryRejection`'s
/// plain-text body.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    AxumQuery<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match AxumQuery::<T>::from_request_parts(parts, state).await {
            Ok(AxumQuery(value)) => Ok(Self(value)),
            Err(rejection) => Err(query_rejection_to_canonical(&rejection)),
        }
    }
}

/// Maps a `QueryRejection` to a `CanonicalError`. Not a `From` impl - both
/// types are foreign to this crate, so the orphan rule forbids it.
///
/// `QueryRejection` is `#[non_exhaustive]` (today it has exactly one
/// variant, `FailedToDeserializeQueryString`), but unlike
/// `extract::json`, the mapping below was never variant-specific in the
/// first place - it's driven entirely by `.status()`/`.body_text()` with one
/// fixed code. An unknown future variant is handled correctly by that same
/// logic with nothing invented, so there's no panic - only a log line for
/// visibility when the variant isn't the one this function was written
/// against.
fn query_rejection_to_canonical(rejection: &QueryRejection) -> CanonicalError {
    if !matches!(rejection, QueryRejection::FailedToDeserializeQueryString(_)) {
        tracing::error!(
            rejection = %rejection,
            "extract::Query: unhandled QueryRejection variant, falling back to the same status-driven classification"
        );
    }
    rejection_to_canonical(
        "query",
        "invalid_query_string",
        rejection.status().as_u16(),
        rejection.body_text(),
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::get;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::Query;

    const RESOURCE_TYPE: &str = "gts.cf.core.http.request.v1~";
    const INVALID_ARGUMENT_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~";

    #[derive(Debug, Deserialize)]
    struct Filter {
        page: u32,
    }

    fn app() -> Router {
        Router::new().route(
            "/items",
            get(|Query(f): Query<Filter>| async move { axum::Json(f.page) }),
        )
    }

    async fn get_uri(uri: &str) -> axum::response::Response {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        app().oneshot(req).await.unwrap()
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("response body is valid JSON")
    }

    #[tokio::test]
    async fn valid_query_extracts_normally() {
        // Echoes the extracted `page` back and asserts it, not just the
        // status - a `Query<T>` that silently yielded a wrong or defaulted
        // value would still return 200 and pass a status-only assertion.
        let res = get_uri("/items?page=1").await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json, json!(1));
    }

    #[tokio::test]
    async fn non_numeric_field_returns_400_problem() {
        let res = get_uri("/items?page=not-a-number").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let json = body_json(res).await;
        assert_eq!(
            json,
            json!({
                "type": INVALID_ARGUMENT_TYPE,
                "title": "Invalid Argument",
                "status": 400,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "query",
                        "description": "Failed to deserialize query string: page: invalid digit found in string",
                        "reason": "invalid_query_string",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn missing_required_field_returns_400_problem() {
        let res = get_uri("/items").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let json = body_json(res).await;
        assert_eq!(
            json,
            json!({
                "type": INVALID_ARGUMENT_TYPE,
                "title": "Invalid Argument",
                "status": 400,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "query",
                        "description": "Failed to deserialize query string: missing field `page`",
                        "reason": "invalid_query_string",
                    }],
                },
            })
        );
    }
}
