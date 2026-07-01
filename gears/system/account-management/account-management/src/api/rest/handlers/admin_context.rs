//! REST handler for `GET /account-management/v1/admin/context`.
//!
//! Startup admin context for the admin panel: projects the authenticated
//! caller's [`SecurityContext`] (subject id, type, home tenant) plus a
//! derived admin **mode** and coarse **capabilities** hint into the wire
//! [`AdminContextDto`]. Pure context projection — no service, no domain
//! logic, no I/O. Mode/capabilities derive from the `subject_type` role
//! marker; the backend remains the final authority on every action.

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use tracing::field::Empty;

use crate::api::rest::dto::AdminContextDto;

/// `GET /account-management/v1/admin/context`
///
/// Returns the authenticated subject's identity, home tenant, admin mode,
/// and capability hints, read from the request [`SecurityContext`]. Always
/// succeeds for an authenticated caller; the `.authenticated()` route gate
/// produces 401 upstream when the bearer token is missing or invalid.
///
/// # Errors
///
/// Never returns `Err` for a caller that reaches this handler;
/// unauthenticated requests are rejected upstream by the
/// `.authenticated()` route gate.
#[tracing::instrument(skip(ctx), fields(request_id = Empty))]
pub async fn get_admin_context(
    Extension(ctx): Extension<SecurityContext>,
) -> ApiResult<Json<AdminContextDto>> {
    Ok(Json(AdminContextDto::from_security_context(&ctx)))
}

#[cfg(test)]
#[path = "admin_context_tests.rs"]
mod tests;
