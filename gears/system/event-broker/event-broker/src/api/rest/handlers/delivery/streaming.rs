//! `GET /v1/events:stream`, `GET /v1/events:sse` (`DESIGN.md:589`,
//! `event-broker-consumption-frames`, baseline subset only - see
//! `domain/delivery.rs`'s module doc comment).

// [todo]: I don't see any tests for this file

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::Query;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio_stream::StreamExt;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use super::dto::{FrameDto, StreamQuery};
use crate::api::rest::state::HandlerState;
use crate::domain::delivery::{Frame, StreamHandle};

/// Wraps a `StreamHandle` as a `Stream<Item = Frame>`, keeping the whole
/// handle - including its active-stream-marker drop guard - alive for as
/// long as the stream itself is polled, rather than only for the handler
/// function's own stack frame. `StreamHandle`'s own doc comment is explicit
/// that the marker "follows returned stream lifetime": destructuring just
/// `handle.frames` into `ReceiverStream::new(...)` and letting the rest of
/// `handle` drop at the end of the handler body clears the marker almost
/// immediately after `state.delivery.stream()` returns - long before a real
/// client has read anything - making `StreamingInProgress` unenforceable in
/// practice (caught by `handlers/delivery/subscriptions_tests.rs`'s real two-request
/// round trip, task 11.5).
struct GuardedFrames(StreamHandle);

impl tokio_stream::Stream for GuardedFrames {
    type Item = Frame;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Frame>> {
        self.0.frames.poll_recv(cx)
    }
}

const MULTIPART_BOUNDARY: &str = "event-broker-frame-boundary";

/// Maps one domain `Frame` to a multipart/mixed part (`stream_events`'s wire
/// shape) - a `--boundary`/`Content-Type`/JSON-body/`\r\n` part, matching
/// `event-broker-consumption-frames`.
// `Body::from_stream` requires `Stream<Item = Result<_, E>>` - the `Result`
// wrapper is a call-site contract, not a spurious `Ok`.
#[allow(clippy::unnecessary_wraps)]
fn frame_to_multipart_part(frame: Frame) -> Result<Bytes, Infallible> {
    let dto = FrameDto::from(frame);
    let json = serde_json::to_vec(&dto).unwrap_or_default();
    let part = [
        format!("--{MULTIPART_BOUNDARY}\r\nContent-Type: application/json\r\n\r\n").into_bytes(),
        json,
        b"\r\n".to_vec(),
    ]
    .concat();
    Ok(Bytes::from(part))
}

/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `DeliveryService::stream` produces (subscription not found,
/// `PositionsNotSet`, `StreamingInProgress`).
pub async fn stream_events(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Query(query): Query<StreamQuery>,
) -> ApiResult<impl IntoResponse> {
    let handle = state.delivery.stream(&ctx, query.subscription_id).await?;
    let stream = GuardedFrames(handle).map(frame_to_multipart_part);
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            format!("multipart/mixed; boundary={MULTIPART_BOUNDARY}"),
        )],
        Body::from_stream(stream),
    ))
}

/// Maps one domain `Frame` to an SSE event (`sse_events`'s wire shape) - the
/// `event:` name is the frame's own `kind` discriminant, the data is the
/// frame DTO itself; falls back to an empty `heartbeat` event if the frame
/// somehow fails to serialize (`SseEvent::json_data`'s only failure mode).
// `Sse::new` requires `Stream<Item = Result<Event, E>>` - same contract as
// `frame_to_multipart_part` above.
#[allow(clippy::unnecessary_wraps)]
fn frame_to_sse_event(frame: Frame) -> Result<SseEvent, Infallible> {
    let dto = FrameDto::from(frame);
    let kind = match &dto {
        FrameDto::Event { .. } => "event",
        FrameDto::Heartbeat { .. } => "heartbeat",
        FrameDto::Topology { .. } => "topology",
        FrameDto::Control { .. } => "control",
    };
    Ok(SseEvent::default()
        .event(kind)
        .json_data(dto)
        .unwrap_or_else(|_| SseEvent::default().event("heartbeat").data("{}")))
}

/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `DeliveryService::stream` produces (subscription not found,
/// `PositionsNotSet`, `StreamingInProgress`).
pub async fn sse_events(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Query(query): Query<StreamQuery>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>>> {
    let handle = state.delivery.stream(&ctx, query.subscription_id).await?;
    let stream = GuardedFrames(handle).map(frame_to_sse_event);
    // Heartbeats are already real domain frames on the documented cadence -
    // no need for axum's own comment-based keepalive on top.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default().text("")))
}
