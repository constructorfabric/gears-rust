//! REST handlers: thin pass-through to the domain service.

use std::sync::Arc;

use axum::extract::Extension;
use axum::response::{IntoResponse, Response};
use llm_gateway_sdk::LlmGatewayError;
use toolkit::api::canonical_prelude::Json;
use toolkit_security::SecurityContext;

use super::dto::{CreateResponseRequest, ResponseResourceDto};
use super::error::error_to_problem;
use crate::domain::Service;

/// `POST /llm-gateway/v1/responses` (`stream: false`): `200` JSON
/// `ResponseResource`, or an `application/problem+json` Problem.
///
/// `stream: true` is not served yet (streaming arrives in a later slice) and
/// is rejected before any pipeline work.
pub async fn handle_create_response(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Json(req): Json<CreateResponseRequest>,
) -> Response {
    if req.stream.unwrap_or(false) {
        return error_to_problem(
            &LlmGatewayError::capability_not_supported("streaming"),
            None,
        )
        .into_response();
    }
    match service.create_response_traced(&ctx, req.body).await {
        Ok(response) => Json(ResponseResourceDto { response }).into_response(),
        Err(failure) => {
            error_to_problem(&failure.error, Some(&failure.response_id)).into_response()
        }
    }
}
