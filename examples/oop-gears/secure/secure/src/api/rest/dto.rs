//! REST DTOs for the secure gear (transport-specific: serde + utoipa).

/// Response for `GET /secure/v1/whoami`.
///
/// The values are read from the [`SecurityContext`](toolkit_security::SecurityContext)
/// reconstructed by the OoP pod's own `security_context_middleware`, proving the
/// forwarded bearer was re-validated locally.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct WhoAmIResponse {
    /// The authenticated subject (user/service) id.
    pub subject_id: String,
    /// The subject's tenant id.
    pub tenant_id: String,
    /// The token scopes carried by the reconstructed context.
    pub scopes: Vec<String>,
    /// The gear instance that served the request (proves it came from the OoP
    /// pod, not the gateway).
    pub served_by: String,
}
