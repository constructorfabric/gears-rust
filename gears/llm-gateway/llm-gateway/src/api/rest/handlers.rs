//! REST handlers: thin pass-through to the domain service.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::Extension;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use llm_gateway_sdk::{ResponseEventStream, StreamingEvent};
use toolkit::api::canonical_prelude::Json;
use toolkit_security::SecurityContext;

use super::dto::{
    CreateEmbeddingRequest, CreateResponseRequest, EmbeddingResponseDto, ResponseResourceDto,
};
use super::error::error_to_problem;
use crate::domain::Service;

/// `POST /llm-gateway/v1/responses`.
///
/// `stream: false`: `200` JSON `ResponseResource`. `stream: true`: `200`
/// `text/event-stream` of streaming events closed by `data: [DONE]`. Failures
/// before the first byte are `application/problem+json` Problems.
pub async fn handle_create_response(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Json(req): Json<CreateResponseRequest>,
) -> Response {
    if req.stream.unwrap_or(false) {
        return match service.create_response_stream_traced(&ctx, req.body).await {
            Ok(stream) => sse_response(stream).into_response(),
            Err(failure) => {
                error_to_problem(&failure.error, Some(&failure.response_id)).into_response()
            }
        };
    }
    match service.create_response_traced(&ctx, req.body).await {
        Ok(response) => Json(ResponseResourceDto { response }).into_response(),
        Err(failure) => {
            error_to_problem(&failure.error, Some(&failure.response_id)).into_response()
        }
    }
}

/// `POST /llm-gateway/v1/embeddings`: `200` JSON `EmbeddingResponse`, or an
/// `application/problem+json` Problem carrying the allocated request id.
pub async fn handle_create_embedding(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Json(req): Json<CreateEmbeddingRequest>,
) -> Response {
    match service.create_embedding_traced(&ctx, req.request).await {
        Ok(response) => Json(EmbeddingResponseDto { response }).into_response(),
        Err(failure) => error_to_problem(&failure.error, Some(&failure.request_id)).into_response(),
    }
}

/// SSE body of a supervised stream: one `event: <type>` frame per event, then
/// the `data: [DONE]` sentinel (no event line). Dropping the response drops the
/// stream, which cancels the request.
fn sse_response(
    stream: ResponseEventStream,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let events = stream.map(|event| Ok(frame(&event)));
    let done = futures_util::stream::once(async { Ok(Event::default().data("[DONE]")) });
    Sse::new(events.chain(done)).keep_alive(KeepAlive::default())
}

fn frame(event: &StreamingEvent) -> Event {
    // The SSE event name is the `type` discriminator of the payload.
    match serde_json::to_value(event) {
        Ok(value) => {
            let name = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                // A CR/LF in a provider-supplied name would break the framing
                // (and panic in `Event::event`): send the data line only.
                .filter(|name| !name.contains(['\r', '\n']))
                .map(str::to_owned);
            let frame = Event::default().data(value.to_string());
            match name {
                Some(name) => frame.event(name),
                None => frame,
            }
        }
        Err(err) => {
            tracing::error!(error = %err, "streaming event could not be serialized");
            Event::default().comment("event dropped")
        }
    }
}
