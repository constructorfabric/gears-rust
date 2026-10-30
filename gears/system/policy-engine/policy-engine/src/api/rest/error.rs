//! Canonical errors the transport detects before a request ever reaches
//! [`PolicyManagementClientV1`](policy_engine_sdk::management::PolicyManagementClientV1).
//!
//! A request with no security context, or an anonymous one, is
//! `unauthenticated` (401, reason
//! [`SECURITY_CONTEXT_REQUIRED`](policy_engine_sdk::error::reason::SECURITY_CONTEXT_REQUIRED)).
//! Every domain refusal is the client's typed
//! [`ManagementError`](policy_engine_sdk::management::ManagementError) and
//! reaches the wire as its derived `Problem`, `error_code` included.

use axum::body::HttpBody;
use axum::extract::{Extension, FromRequest, Request};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use policy_engine_sdk::error::reason::SECURITY_CONTEXT_REQUIRED;
use policy_engine_sdk::management::ManagementError;
use serde::de::DeserializeOwned;
use toolkit::api::canonical_prelude::extract;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::SecurityContext;

/// A handler's refusal: the `Problem` a domain refusal or a transport
/// refusal such as [`unauthenticated`] converts into with `?`, boxed because
/// it is large.
#[derive(Debug)]
pub struct ApiError(Box<Problem>);

impl From<ManagementError> for ApiError {
    fn from(err: ManagementError) -> Self {
        Self(Box::new(err.into()))
    }
}

impl From<CanonicalError> for ApiError {
    fn from(err: CanonicalError) -> Self {
        Self(Box::new(err.into()))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}

/// The result of a handler.
pub type ApiResult<T> = Result<T, ApiError>;

/// No security context, or an anonymous one: the request never reached
/// [`PolicyManagementClientV1`](policy_engine_sdk::management::PolicyManagementClientV1),
/// which cannot represent the absence of a caller.
#[must_use]
pub fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason(SECURITY_CONTEXT_REQUIRED)
        .create()
}

/// Requires a present, non-anonymous `SecurityContext` extension. Every
/// route here is `.authenticated()`; a request that reaches a handler
/// without one (in a router built without the platform's auth layer, as in a
/// unit test) is refused the same way a domain check downstream would refuse
/// it, rather than surfacing as axum's generic missing-extension `500`.
///
/// # Errors
///
/// [`unauthenticated`] when `ctx` is absent or the context is anonymous.
pub fn require_context(
    ctx: Option<Extension<SecurityContext>>,
) -> Result<SecurityContext, CanonicalError> {
    ctx.map(|Extension(ctx)| ctx)
        .filter(|ctx| !ctx.is_anonymous())
        .ok_or_else(unauthenticated)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn require_context_rejects_absent_and_anonymous() {
        assert_eq!(unauthenticated().status_code(), 401);
        assert!(require_context(None).is_err());
        let anonymous = SecurityContext::builder()
            .subject_id(uuid::Uuid::nil())
            .subject_tenant_id(uuid::Uuid::nil())
            .build()
            .expect("anonymous ctx");
        assert!(anonymous.is_anonymous());
        assert!(require_context(Some(Extension(anonymous))).is_err());
        let real = SecurityContext::builder()
            .subject_id(uuid::Uuid::from_u128(1))
            .subject_tenant_id(uuid::Uuid::from_u128(2))
            .build()
            .expect("ctx");
        assert!(require_context(Some(Extension(real))).is_ok());
    }
}

/// A JSON request body the caller may leave out: `None` for a request with
/// no `Content-Type` and a body known to be empty. Anything else is read as
/// [`extract::Json`](toolkit::api::canonical_prelude::extract::Json) reads
/// it, so a body without a JSON content type or with malformed JSON is still
/// refused as a `Problem`, never silently ignored.
#[derive(Debug, Clone, Copy, Default)]
pub struct OptionalJson<T>(pub Option<T>);

impl<T, S> FromRequest<S> for OptionalJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !req.headers().contains_key(header::CONTENT_TYPE)
            && req.body().size_hint().exact() == Some(0)
        {
            return Ok(Self(None));
        }
        let extract::Json(body) = extract::Json::<T>::from_request(req, state).await?;
        Ok(Self(Some(body)))
    }
}
