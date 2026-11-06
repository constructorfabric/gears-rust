//! `POST /v1/events`, `POST /v1/events:batch` (`DESIGN.md:584`,
//! `docs/schemas/gts.cf.core.events.event.v1~.schema.json`).

use std::time::Duration;

use axum::Extension;
use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use super::dto::{PublishBatchRequest, PublishEventRequest};
use crate::api::rest::state::HandlerState;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::{BatchAck, PublishAck, PublishRequest, SyncPublishAck};

/// The wait a `Prefer: wait` publish asks for (RFC 7240), or `None` for the
/// default async publish (`respond-async` or no header). A bare `wait` with no
/// value asks to wait as long as the broker allows - [`Duration::MAX`], which
/// the ingest service clamps to its configured cap exactly as it clamps a
/// `wait=N`. A `wait=<non-numeric>` value is a malformed preference, rejected
/// as a `400` rather than silently reinterpreted. The cap itself is applied
/// downstream, by the ingest service, so this parse needs no configuration.
fn requested_wait(headers: &axum::http::HeaderMap) -> Result<Option<Duration>, DomainError> {
    for token in headers
        .get_all("Prefer")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
    {
        let mut parts = token.trim().splitn(2, '=');
        let name = parts.next().unwrap_or_default().trim();
        if !name.eq_ignore_ascii_case("wait") {
            continue;
        }
        return match parts.next().map(str::trim) {
            None | Some("") => Ok(Some(Duration::MAX)),
            // A whole number of seconds. A value too large for the seconds field
            // is not malformed - it just asks for more than the broker allows,
            // so it saturates and the ingest service clamps it to the cap, the
            // same as any over-cap `wait`. Only a value that is not a
            // non-negative integer at all is a malformed preference (`400`).
            Some(secs) if secs.bytes().all(|b| b.is_ascii_digit()) && !secs.is_empty() => Ok(Some(
                secs.parse::<u64>()
                    .map_or(Duration::MAX, Duration::from_secs),
            )),
            Some(_) => Err(DomainError::Validation {
                code: ErrorCode::InvalidHeader,
                message: "Prefer: wait expects a non-negative whole number of seconds".to_owned(),
            }),
        };
    }
    Ok(None)
}

/// The HTTP status a synchronous publish's outcome answers (DESIGN.md:1335):
/// `201` once stored, `202` when the wait elapsed first (the events are still
/// enqueued), `200` when every event was a duplicate.
fn sync_status(ack: SyncPublishAck) -> StatusCode {
    match ack {
        SyncPublishAck::Stored => StatusCode::CREATED,
        SyncPublishAck::TimedOut => StatusCode::ACCEPTED,
        SyncPublishAck::Duplicate => StatusCode::OK,
    }
}

/// A synchronous publish's response, carrying `Preference-Applied: wait=<N>`
/// (RFC 7240 §3) so a client can tell a `202` the broker produced after
/// honouring the preference from one where the preference was ignored. `applied`
/// is the requested wait clamped to the broker's ceiling, in whole seconds.
fn sync_response(ack: SyncPublishAck, applied: std::time::Duration) -> Response {
    (
        sync_status(ack),
        [(
            axum::http::header::HeaderName::from_static("preference-applied"),
            format!("wait={}", applied.as_secs()),
        )],
        (),
    )
        .into_response()
}

/// The wait the broker will actually honour: the requested hold clamped to the
/// configured ceiling (a bare `wait` requests [`Duration::MAX`], so it reports
/// the ceiling).
fn applied_wait(state: &HandlerState, requested: Duration) -> Duration {
    requested.min(state.ingest.max_sync_wait())
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
///
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `IngestService::publish_event` produces (an unauthorized event type or
/// tenant, topic/event-type not found, payload validation, sequence
/// violation, unknown producer), or `400` when the body carries a
/// read-only/unknown field or `Prefer: wait` names a malformed value.
pub async fn publish_event(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let req: PublishEventRequest = parse_body(&body)?;
    // `Prefer: wait=N` holds the response until the backend stores the event
    // (`201`) or the wait elapses (`202`); a duplicate is `200`. The default is
    // asynchronous: a single enqueue, answered `202`/`200`.
    if let Some(wait) = requested_wait(&headers)? {
        let ack = state
            .ingest
            .publish_event_sync(&ctx, req.try_into()?, wait)
            .await?;
        return Ok(sync_response(ack, applied_wait(&state, wait)));
    }
    let status = match state.ingest.publish_event(&ctx, req.try_into()?).await? {
        // A lost-ack retry of the producer chain's current head: the event
        // is already durable, so nothing new is written and the response
        // carries no body - `200 OK`, distinct from the `202 Accepted` a
        // newly admitted event gets.
        PublishAck::Duplicate(_) => StatusCode::OK,
        PublishAck::Accepted(_) => StatusCode::ACCEPTED,
    };
    Ok(status.into_response())
}

/// Every answer is status only (`docs/openapi.yaml`). A batch is
/// all-or-nothing: any one event's rejection rejects the whole request
/// (`IngestService::publish_batch`, feature 0001 §5.2).
///
/// # Errors
///
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `IngestService::publish_batch` produces (an empty batch, a repeated event
/// id, a malformed run, mixed topics, batch too large, an unauthorized event
/// type or tenant, topic/event-type not found, unknown producer, payload
/// validation, a sequence violation, or an unavailable store), or `400` when
/// the body carries a read-only/unknown field or `Prefer: wait` names a
/// malformed value.
pub async fn publish_batch(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let req: PublishBatchRequest = parse_body(&body)?;
    let requests = req
        .events
        .into_iter()
        .map(PublishRequest::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    // `Prefer: wait=N` applies to the batch as a whole, with the status codes
    // of a single publish (feature 0001 §5.1).
    if let Some(wait) = requested_wait(&headers)? {
        let ack = state
            .ingest
            .publish_batch_sync(&ctx, requests, wait)
            .await?;
        return Ok(sync_response(ack, applied_wait(&state, wait)));
    }
    let status = match state.ingest.publish_batch(&ctx, requests).await? {
        // Every event a duplicate - a lost-ack resend: nothing new is written
        // (feature 0001 §5.2 "Outcome").
        BatchAck::Duplicate => StatusCode::OK,
        BatchAck::Accepted => StatusCode::ACCEPTED,
    };
    Ok(status.into_response())
}
