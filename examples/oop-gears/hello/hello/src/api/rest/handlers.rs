//! REST handlers for the hello gear.

use toolkit::api::canonical_prelude::*;

use super::dto::PingResponse;

/// Handler for `GET /hello/v1/ping`.
///
/// Anonymous (no auth) and self-contained — returns a fixed greeting plus the
/// process id so a caller can confirm the response came from the OoP pod rather
/// than the gateway itself.
pub async fn handle_ping() -> ApiResult<Json<PingResponse>> {
    Ok(Json(PingResponse {
        message: "pong".to_owned(),
        served_by: format!("hello-oop (pid {})", std::process::id()),
    }))
}
