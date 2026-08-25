//! `Json<T>` - a drop-in `axum::Json<T>` replacement, usable as both a
//! request extractor and a response body, whose extraction failures render
//! as `application/problem+json` (RFC 9457 `Problem`) instead of axum's
//! default plain-text rejection body.
//!
//! `axum::Json<T>`'s `FromRequest` impl resolves and, on failure, calls
//! `.into_response()` on its own `JsonRejection` directly - inside axum's
//! generated `Handler::call`, before the handler body ever runs. No
//! handler-level `CanonicalError` conversion is ever in the loop for a
//! malformed body. `Json<T>` closes that gap by making the extractor's own
//! `Rejection` a `CanonicalError`.
//!
//! `IntoResponse` and `From<T>` delegate to `axum::Json`'s own impls, so the
//! common `Json(body): Json<Req> -> ... -> Json(resp)` handler shape
//! compiles unchanged when swapping the import from `axum::Json` to this
//! type.

use axum::Json as AxumJson;
use axum::extract::{FromRequest, Request};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use toolkit_canonical_errors::CanonicalError;

use super::error::json_rejection_to_canonical;

/// Drop-in replacement for `axum::Json<T>` as a handler parameter or
/// response body. Extraction success and response serialization are
/// identical to `axum::Json<T>`; extraction failure produces a
/// `CanonicalError` (rendered as `Problem` by its existing `IntoResponse`
/// impl) instead of `JsonRejection`'s plain-text body.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match AxumJson::<T>::from_request(req, state).await {
            Ok(AxumJson(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection_to_canonical(&rejection)),
        }
    }
}

impl<T> From<T> for Json<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        AxumJson(self.0).into_response()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode, header};
    use axum::routing::post;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::Json;

    const RESOURCE_TYPE: &str = "gts.cf.core.http.request.v1~";
    const INVALID_ARGUMENT_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~";

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Widget {
        name: String,
    }

    fn app() -> Router {
        Router::new().route(
            "/widgets",
            post(|Json(w): Json<Widget>| async move { (StatusCode::CREATED, Json(w)) }),
        )
    }

    async fn post_body(body: &'static str, content_type: &str) -> axum::response::Response {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/widgets")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap();
        app().oneshot(req).await.unwrap()
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("response body is valid JSON")
    }

    #[tokio::test]
    async fn valid_body_extracts_normally() {
        // Echoes the extracted value back via the `IntoResponse` impl and
        // asserts it, not just the status - a `Json<T>` that silently
        // yielded a wrong, empty, or defaulted payload would still return
        // 201 and pass a status-only assertion.
        let res = post_body(r#"{"name":"a"}"#, "application/json").await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let json = body_json(res).await;
        assert_eq!(json, json!({"name": "a"}));
    }

    #[tokio::test]
    async fn usable_as_a_response_body() {
        // The common `Json(body): Json<Req> -> ... -> Json(resp)` handler
        // shape must compile and round-trip unchanged from `axum::Json`.
        let app = Router::new().route(
            "/widgets",
            post(|Json(w): Json<Widget>| async move { Json(w) }),
        );
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/widgets")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"a"}"#))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let json = body_json(res).await;
        assert_eq!(json, json!({"name": "a"}));
    }

    #[tokio::test]
    async fn malformed_json_returns_400_problem() {
        let res = post_body("{not-json}", "application/json").await;
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
                        "field": "body",
                        "description": "Failed to parse the request body as JSON: key must be a string at line 1 column 2",
                        "reason": "json_syntax_error",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn unknown_field_returns_422_problem_with_code() {
        let res = post_body(r#"{"name":"a","extra":1}"#, "application/json").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
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
                "status": 422,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "body",
                        "description": "Failed to deserialize the JSON body into the target type: extra: unknown field `extra`, expected `name` at line 1 column 19",
                        "reason": "invalid_json_body",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn invalid_enum_variant_returns_422_problem() {
        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[allow(dead_code)]
        enum Mode {
            Monotonic,
            Stateless,
        }

        #[derive(Debug, Deserialize)]
        struct WithMode {
            #[allow(dead_code)]
            mode: Mode,
        }

        let app = Router::new().route(
            "/producers",
            post(|Json(_w): Json<WithMode>| async { StatusCode::CREATED }),
        );
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/producers")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"mode":"not_a_real_mode"}"#))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();

        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
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
                "status": 422,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "body",
                        "description": "Failed to deserialize the JSON body into the target type: mode: unknown variant `not_a_real_mode`, expected `monotonic` or `stateless` at line 1 column 25",
                        "reason": "invalid_json_body",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn missing_content_type_returns_415_problem() {
        let res = post_body(r#"{"name":"a"}"#, "text/plain").await;
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
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
                "status": 415,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "body",
                        "description": "Expected request with `Content-Type: application/json`",
                        "reason": "missing_json_content_type",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn oversized_body_returns_problem_with_axum_status() {
        use axum::extract::DefaultBodyLimit;

        let app = Router::new()
            .route(
                "/widgets",
                post(|Json(_w): Json<Widget>| async { StatusCode::CREATED }),
            )
            .layer(DefaultBodyLimit::max(4));

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/widgets")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"a"}"#))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();

        // `DefaultBodyLimit::max` reports an over-limit body as a
        // `BytesRejection(LengthLimitError)`, which axum resolves to exactly
        // 413 - assert that precisely rather than any 4xx.
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
                "status": 413,
                "detail": "Request validation failed",
                "context": {
                    "resource_type": RESOURCE_TYPE,
                    "field_violations": [{
                        "field": "body",
                        "description": "Failed to buffer the request body: length limit exceeded",
                        "reason": "json_body_read_error",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn usable_via_canonical_prelude_glob_import() {
        // No explicit `use super::Json` here - only the prelude's glob
        // import, mirroring how a handler module actually adopts it. The
        // prelude re-exports the `extract` module (not a flattened
        // name), so the type is reached as `extract::Json`.
        use crate::api::canonical_prelude::*;

        #[derive(Debug, Deserialize)]
        struct FromPrelude {
            #[allow(dead_code)]
            name: String,
        }

        let app = Router::new().route(
            "/via-prelude",
            post(|extract::Json(_w): extract::Json<FromPrelude>| async { StatusCode::CREATED }),
        );
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/via-prelude")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"a"}"#))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
    }
}
