//! `POST /v1/events`, `POST /v1/events:batch` (`DESIGN.md:584`,
//! `docs/schemas/gts.cf.core.events.event.v1~.schema.json`).

use axum::Extension;
use axum::body::Bytes;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use super::dto::{PublishBatchRequest, PublishEventRequest};
use crate::api::rest::error::EventBrokerResourceError;
use crate::api::rest::state::HandlerState;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::{PublishAck, PublishRequest};

/// `true` when the caller opted into synchronous persistence via the standard
/// `Prefer: wait` header (RFC 7240) - asking the broker to hold the response
/// until the backend has confirmed the event is persisted. Publish is
/// asynchronous by default (`202 Accepted`: ingest persists to its own store,
/// acks the producer, then delivers to the backend out of band); the
/// synchronous path is not built yet, so requesting it is `501`. `Prefer:
/// respond-async` names the default behaviour and is accepted as a no-op.
fn wants_sync_wait(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get_all("Prefer")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| {
            token
                .trim()
                .split('=')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("wait")
        })
}

/// `501` for a `Prefer: wait` (synchronous) publish. Carries no id or user
/// input - the caller only needs to know the mode is not available yet.
fn sync_wait_unimplemented() -> CanonicalError {
    EventBrokerResourceError::unimplemented(
        "synchronous publish (Prefer: wait) is not implemented yet",
    )
    .create()
}

/// Parse a publish body ourselves rather than through axum's `Json` extractor.
/// The request DTOs are `#[serde(deny_unknown_fields)]`, so a body carrying a
/// read-only field (`partition`/`sequence`/`sequence_time`) or any unknown key
/// is a malformed request the producer must fix - a `400`, not the `422` axum's
/// `JsonRejection` would emit for the same deserialize failure. `422` is
/// reserved for payload-*schema* violations, which are decided later in the
/// domain against the event type's `data_schema`.
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, DomainError> {
    serde_json::from_slice::<T>(body).map_err(|err| DomainError::Validation {
        code: ErrorCode::InvalidBody,
        message: format!("invalid JSON body: {err}"),
    })
}

/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `IngestService::publish_event` produces (topic/event-type not found,
/// payload validation, sequence violation, unknown producer), `400` when the
/// body carries a read-only/unknown field, or `501` when the caller requests
/// synchronous persistence via `Prefer: wait`.
pub async fn publish_event(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    if wants_sync_wait(&headers) {
        return Err(sync_wait_unimplemented());
    }
    let req: PublishEventRequest = parse_body(&body)?;
    Ok(
        match state.ingest.publish_event(&ctx, req.try_into()?).await? {
            // A lost-ack retry of the producer chain's current head: the event
            // is already durable, so nothing new is written and the response
            // carries no body - `200 OK`, distinct from the `202 Accepted` a
            // newly admitted event gets.
            PublishAck::Duplicate(_) => StatusCode::OK,
            PublishAck::Accepted(_) => StatusCode::ACCEPTED,
        },
    )
}

/// `docs/openapi.yaml` documents no response body for this endpoint (202/
/// 400/403/412/413, status only) - `BatchResult`'s per-event
/// accepted/failed detail isn't surfaced on success, matching that
/// documented contract exactly rather than inventing an undocumented body
/// shape. A batch is all-or-nothing: any one event's rejection rejects the
/// whole request (`IngestService::publish_batch`).
///
/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `IngestService::publish_batch` produces (mixed topics, batch too large,
/// payload validation, or a sequence violation), `400` when the body carries
/// a read-only/unknown field, or `501` when the caller requests synchronous
/// persistence via `Prefer: wait`.
pub async fn publish_batch(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    if wants_sync_wait(&headers) {
        return Err(sync_wait_unimplemented());
    }
    let req: PublishBatchRequest = parse_body(&body)?;
    let requests = req
        .events
        .into_iter()
        .map(PublishRequest::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    state.ingest.publish_batch(&ctx, requests).await?;
    Ok(StatusCode::ACCEPTED)
}
