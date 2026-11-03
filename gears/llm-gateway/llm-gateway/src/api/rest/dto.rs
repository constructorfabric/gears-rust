//! Wire DTOs. The SDK models stay free of `utoipa::ToSchema`, so the REST
//! layer wraps them in thin DTOs.

use llm_gateway_sdk::{CreateResponseBody, ResponseResource};

/// `POST /responses` request: the Open Responses body plus the REST-only
/// `stream` flag.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateResponseRequest {
    /// `true` selects the SSE streaming variant (later slice).
    pub stream: Option<bool>,
    /// The Open Responses create-response body.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub body: CreateResponseBody,
}

/// `POST /responses` JSON response body (`stream: false`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ResponseResourceDto {
    /// The Open Responses response object.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub response: ResponseResource,
}
