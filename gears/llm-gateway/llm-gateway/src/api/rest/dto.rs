//! Wire DTOs. The SDK models stay free of `utoipa::ToSchema`, so the REST
//! layer wraps them in thin DTOs.

use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, ResponseResource, StreamingEvent,
};

/// `POST /responses` request: the Open Responses body plus the REST-only
/// `stream` flag.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateResponseRequest {
    /// `true` selects the SSE streaming variant (`text/event-stream`).
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

/// One SSE `data:` payload of `POST /responses` (`stream: true`): a
/// `type`-discriminated Open Responses streaming event. Used for the `OpenAPI`
/// schema; events are serialized from the SDK type directly.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct StreamingEventDto {
    /// The streaming event.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub event: StreamingEvent,
}

/// `POST /embeddings` request body (the SDK `EmbeddingRequest`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateEmbeddingRequest {
    /// The embedding request.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub request: EmbeddingRequest,
}

/// `POST /embeddings` response body (the SDK `EmbeddingResponse`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EmbeddingResponseDto {
    /// The embedding response.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub response: EmbeddingResponse,
}
