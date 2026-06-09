//! REST handler for `GET /account-management/v1/me`.
//!
//! Non-tenant-scoped identity reflection: projects the authenticated
//! caller's [`SecurityContext`] (subject id, type, home tenant) into the
//! wire [`MeDto`]. Pure context projection — no service, no domain logic,
//! no I/O. The home tenant's existence/status is intentionally NOT checked
//! here; AM trusts `subject_tenant_id` from the validated token exactly as
//! the tenant-scoped authz subtree does.

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use tracing::field::Empty;

use crate::api::rest::dto::MeDto;

/// `GET /account-management/v1/me`
///
/// Returns the authenticated subject's identity and home tenant, read
/// from the request [`SecurityContext`]. Always succeeds for an
/// authenticated caller; the `.authenticated()` route gate produces 401
/// upstream when the bearer token is missing or invalid.
///
/// # Errors
///
/// Never returns `Err` for a caller that reaches this handler;
/// unauthenticated requests are rejected upstream by the
/// `.authenticated()` route gate.
#[tracing::instrument(skip(ctx), fields(request_id = Empty))]
pub async fn get_me(Extension(ctx): Extension<SecurityContext>) -> ApiResult<Json<MeDto>> {
    Ok(Json(MeDto::from_security_context(&ctx)))
}

#[cfg(test)]
#[path = "me_tests.rs"]
mod tests;
