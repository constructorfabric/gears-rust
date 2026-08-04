//! REST DTOs for the hello gear (transport-specific: serde + utoipa).

/// Response for `GET /hello/v1/ping`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PingResponse {
    /// A fixed greeting, always `"pong"`.
    pub message: String,
    /// The gear instance that served the request (useful to prove the request
    /// was reverse-proxied to the OoP pod).
    pub served_by: String,
}
