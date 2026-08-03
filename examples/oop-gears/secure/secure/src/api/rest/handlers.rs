//! REST handlers for the secure gear.

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use super::dto::WhoAmIResponse;

/// Handler for `GET /secure/v1/whoami`.
///
/// Authenticated: the OoP pod's `security_context_middleware` has already
/// re-validated the forwarded bearer and inserted the reconstructed
/// [`SecurityContext`] into the request extensions. We echo the resolved
/// identity so a caller can confirm the token was validated **inside the pod**.
pub async fn handle_whoami(
    Extension(ctx): Extension<SecurityContext>,
) -> ApiResult<Json<WhoAmIResponse>> {
    Ok(Json(WhoAmIResponse {
        subject_id: ctx.subject_id().to_string(),
        tenant_id: ctx.subject_tenant_id().to_string(),
        scopes: ctx.token_scopes().to_vec(),
        served_by: format!("secure-oop (pid {})", std::process::id()),
    }))
}
