//! `Path<T>` - a drop-in `axum::extract::Path<T>` replacement whose
//! extraction failures render as `application/problem+json` (RFC 9457
//! `Problem`) instead of axum's default plain-text rejection body. Same
//! class of gap as `extract::Json`, same fix shape - see this module's
//! parent doc comment.

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path as AxumPath};
use axum::http::request::Parts;
use toolkit_canonical_errors::CanonicalError;

use super::error::rejection_to_canonical;

/// Drop-in replacement for `axum::extract::Path<T>` as a handler parameter.
/// Extraction success is identical to `axum::extract::Path<T>`; extraction
/// failure produces a `CanonicalError` instead of `PathRejection`'s
/// plain-text body.
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    AxumPath<T>: FromRequestParts<S, Rejection = PathRejection>,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match AxumPath::<T>::from_request_parts(parts, state).await {
            Ok(AxumPath(value)) => Ok(Self(value)),
            Err(rejection) => Err(path_rejection_to_canonical(&rejection)),
        }
    }
}

/// Maps a `PathRejection` to a `CanonicalError`. Not a `From` impl - both
/// types are foreign to this crate, so the orphan rule forbids it.
///
/// Unlike `extract::json`/`extract::query`, this isn't purely a
/// client-fault mapping: `MissingPathParams` (the extractor used outside a
/// matched route) and `FailedToDeserializePathParams`'s
/// `WrongNumberOfParameters`/`UnsupportedType` kinds are route/type
/// *definition* bugs, not something any client input could trigger - axum
/// itself reports both at `.status() == 500`. `rejection_to_canonical`'s own
/// `>= 500` branch handles this uniformly (mapping to `internal` instead of
/// mislabeling a route/type bug as `invalid_argument`), so no special-casing
/// is needed here at all - every variant, known or not, goes through the
/// same call.
///
/// `PathRejection` is `#[non_exhaustive]`, so this needs a fallback for a
/// variant it doesn't know about - the classification is entirely
/// status-driven, not variant-specific, so an unknown variant is handled
/// correctly with nothing invented. No panic: this just logs the
/// unrecognized variant for visibility.
fn path_rejection_to_canonical(rejection: &PathRejection) -> CanonicalError {
    if !matches!(
        rejection,
        PathRejection::MissingPathParams(_) | PathRejection::FailedToDeserializePathParams(_)
    ) {
        tracing::error!(
            rejection = %rejection,
            "extract::Path: unhandled PathRejection variant, falling back to status-driven classification"
        );
    }
    rejection_to_canonical(
        "path",
        "invalid_path_params",
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
    use toolkit_canonical_errors::Problem;
    use tower::ServiceExt;

    use super::Path;

    const RESOURCE_TYPE: &str = "gts.cf.core.http.request.v1~";
    const INVALID_ARGUMENT_TYPE: &str =
        "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~";
    const INTERNAL_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~";

    #[derive(Debug, Deserialize)]
    struct ItemParams {
        id: u32,
    }

    fn app() -> Router {
        Router::new().route(
            "/items/{id}",
            get(|Path(p): Path<ItemParams>| async move { axum::Json(p.id) }),
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
    async fn valid_path_extracts_normally() {
        // Echoes the extracted `id` back and asserts it, not just the
        // status - a `Path<T>` that silently yielded a wrong or defaulted
        // value would still return 200 and pass a status-only assertion.
        let res = get_uri("/items/42").await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json, json!(42));
    }

    #[tokio::test]
    async fn non_numeric_segment_returns_400_problem() {
        let res = get_uri("/items/not-a-number").await;
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
                        "field": "path",
                        "description": "Invalid URL: Cannot parse `id` with value `not-a-number` to a `u32`",
                        "reason": "invalid_path_params",
                    }],
                },
            })
        );
    }

    #[tokio::test]
    async fn wrong_number_of_path_params_maps_to_internal_not_invalid_argument() {
        // `FailedToDeserializePathParams`'s `WrongNumberOfParameters` kind is
        // a route/type definition bug (the handler declared more path
        // parameters than the route template has), not something client
        // input could trigger - axum reports it at `.status() == 500`, so it
        // must map to `internal`, not `invalid_argument`, per
        // `path_rejection_to_canonical`'s doc comment.
        let app = Router::new().route(
            "/items/{id}",
            get(|Path(_p): Path<(u32, u32)>| async { StatusCode::OK }),
        );
        let req = Request::builder()
            .uri("/items/42")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();

        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(res).await;
        assert_eq!(
            json,
            json!({
                "type": INTERNAL_TYPE,
                "title": "Internal",
                "status": 500,
                "detail": "An internal error occurred. Please retry later.",
                "context": {},
            })
        );
    }

    #[test]
    fn missing_path_params_maps_to_internal_not_invalid_argument() {
        // `MissingPathParams` (the extractor used outside a matched route)
        // is a route/type definition bug, not the client's fault - verified
        // directly against `path_rejection_to_canonical` rather than via
        // real routing, since axum's router always populates path params
        // for any matched route, making this case impractical to trigger
        // end-to-end.
        use axum::extract::rejection::{MissingPathParams, PathRejection};

        let rejection = PathRejection::MissingPathParams(MissingPathParams::default());
        let err = super::path_rejection_to_canonical(&rejection);
        let problem: Problem = err.into();
        let json = serde_json::to_value(&problem).unwrap();

        assert_eq!(
            json,
            json!({
                "type": INTERNAL_TYPE,
                "title": "Internal",
                "status": 500,
                "detail": "An internal error occurred. Please retry later.",
                "context": {},
            })
        );
    }
}
