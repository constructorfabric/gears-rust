//! REST API surface for `cf-chat-engine`.
//!
//! Phase 14 assembles every per-feature handler (Phases 4-13) into a
//! single cohesive [`axum::Router`] via [`OperationBuilder`]. The wiring
//! is split across:
//!
//! - [`dto`] — wire-shape DTOs (`utoipa::ToSchema`).
//! - [`error`] — RFC-9457 mapping (`From<ChatEngineError> for CanonicalError`).
//! - [`handlers`] — thin axum handlers (Phases 4-12).
//! - [`routes`] — `register_routes` and the per-endpoint `OperationBuilder` chains.
//! - [`mod@self`] — NDJSON streaming helper + `WebhookEmitter` trait.
//!
//! ## NDJSON streaming
//!
//! The streaming endpoints (`POST messages`, `POST messages/recreate`,
//! `POST sessions/{id}/summarize`) all flush events as one
//! [`StreamingEventDto`](dto::StreamingEventDto) per line over
//! `application/x-ndjson`. See [`ndjson_response`] for the canonical
//! response builder.
//!
//! ## Webhook emitter
//!
//! The DESIGN webhook protocol (§Webhook Protocol) declares one event
//! type per lifecycle / message transition. The [`WebhookEmitter`] trait
//! defined here exposes a typed method per event so the production wiring
//! in Phase 15 can journal events into a transactional outbox without
//! every call site repeating the `WebhookEvent` enum match. The trait is
//! a strict extension of the domain-layer
//! [`crate::domain::service::WebhookEmitter`] used by session / message
//! services since Phase 4 — the legacy emitter remains the canonical
//! sink and is automatically blanket-impl'd for every implementor of
//! [`WebhookEmitter`] declared here, so no service-layer change is
//! required.
//
// @cpt-cf-chat-engine-api-rest-root:p14
// @cpt-cf-chat-engine-adr-http-client-protocol:p14

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;

pub use routes::register_routes;

use std::convert::Infallible;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use futures::stream::{Stream, StreamExt};
use serde_json::json;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::service::webhook::{
    NoopWebhookEmitter as DomainNoopWebhookEmitter, WebhookEmitter as DomainWebhookEmitter,
    WebhookEvent,
};
use dto::StreamingEventDto;

// ===========================================================================
// NDJSON streaming helper
// ===========================================================================

/// Wrap a fallible streaming-event source into an `application/x-ndjson`
/// chunked HTTP response.
///
/// - `Content-Type: application/x-ndjson`
/// - `Cache-Control: no-cache`
/// - `X-Accel-Buffering: no` (defeats nginx buffering so chunks actually
///   flush over the wire)
///
/// Each successful item is serialized as one JSON object followed by `\n`.
/// If the stream yields `Err(ChatEngineError)`, the helper emits a single
/// terminal `StreamingErrorDto` line carrying the error text and ends the
/// stream — no further events are produced. This mirrors the SDK contract
/// that `StreamingErrorEvent` is terminal.
///
/// Serialization failures degrade gracefully to a best-effort error line so
/// the connection still closes cleanly rather than hanging on a broken
/// sink.
pub fn ndjson_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<StreamingEventDto, ChatEngineError>> + Send + 'static,
{
    let body_stream = stream.map(|item| {
        let mut bytes = match item {
            Ok(evt) => serialize_event_or_fallback(&evt),
            Err(err) => {
                tracing::warn!(error = %err, "ndjson stream terminated with error event");
                serialize_error_line(&err)
            }
        };
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        std::result::Result::<_, Infallible>::Ok(bytes)
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-ndjson"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .header("x-accel-buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "failed to build ndjson response");
            // The header values are static; this branch should never fire,
            // but degrade to a JSON 500 to keep the response shape sane.
            let fallback_body = json!({"type": "internal", "detail": err.to_string()}).to_string();
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )
                .body(Body::from(fallback_body))
                .expect("static fallback response is well-formed")
        })
}

fn serialize_event_or_fallback(evt: &StreamingEventDto) -> Vec<u8> {
    serde_json::to_vec(evt).unwrap_or_else(|err| {
        tracing::error!(error = %err, "failed to serialize StreamingEventDto");
        br#"{"type":"error","error":"internal serialization failure"}"#.to_vec()
    })
}

fn serialize_error_line(err: &ChatEngineError) -> Vec<u8> {
    let line = json!({
        "type": "error",
        "message_id": Uuid::nil(),
        "error": err.to_string(),
    });
    serde_json::to_vec(&line)
        .unwrap_or_else(|_| br#"{"type":"error","error":"unknown"}"#.to_vec())
}

// ===========================================================================
// WebhookEmitter (REST-layer expansion)
// ===========================================================================

/// Typed webhook emitter used by REST handlers and services.
///
/// This trait is an extension of
/// [`crate::domain::service::webhook::WebhookEmitter`]: every implementor
/// of [`WebhookEmitter`] automatically satisfies the domain trait via the
/// blanket impl at the bottom of this file. The split exists so
/// Phase 15's transactional outbox implementation can have ergonomic
/// per-event methods on the wire side while session / message services
/// (Phases 4-12) continue to use the single-method `emit(WebhookEvent)`
/// API they were built against.
///
/// Method coverage (DESIGN §Webhook Protocol):
///
/// - [`emit_session_created`](Self::emit_session_created)
/// - [`emit_message_new`](Self::emit_message_new)
/// - [`emit_message_recreate`](Self::emit_message_recreate)
/// - [`emit_message_aborted`](Self::emit_message_aborted)
/// - [`emit_session_deleted`](Self::emit_session_deleted)
/// - [`emit_session_summary`](Self::emit_session_summary)
/// - [`emit_session_type_health_check`](Self::emit_session_type_health_check)
///
/// Production wiring lands in Phase 15.
#[async_trait]
pub trait WebhookEmitter: Send + Sync {
    async fn emit_session_created(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
        session_type_id: Option<Uuid>,
    ) -> Result<(), ChatEngineError>;

    async fn emit_message_new(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError>;

    async fn emit_message_recreate(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError>;

    async fn emit_message_aborted(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError>;

    async fn emit_session_deleted(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
        hard: bool,
    ) -> Result<(), ChatEngineError>;

    async fn emit_session_summary(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError>;

    async fn emit_session_type_health_check(
        &self,
        session_type_id: Uuid,
    ) -> Result<(), ChatEngineError>;
}

/// No-op REST-layer webhook emitter used by tests and bring-up.
/// Forwards lifecycle events to the domain-layer [`DomainNoopWebhookEmitter`]
/// so downstream metrics / tracing remain consistent across the two
/// trait surfaces.
#[derive(Debug, Default, Clone)]
pub struct NoopWebhookEmitter {
    inner: DomainNoopWebhookEmitter,
}

#[async_trait]
impl WebhookEmitter for NoopWebhookEmitter {
    async fn emit_session_created(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
        session_type_id: Option<Uuid>,
    ) -> Result<(), ChatEngineError> {
        self.inner
            .emit(WebhookEvent::SessionCreated {
                session_id,
                tenant_id: tenant_id.into(),
                user_id: user_id.into(),
                session_type_id,
            })
            .await
    }

    async fn emit_message_new(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError> {
        tracing::debug!(
            event = "message.new",
            %session_id,
            %message_id,
            tenant_id,
            user_id,
            "webhook emitter (noop) — event swallowed",
        );
        Ok(())
    }

    async fn emit_message_recreate(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError> {
        tracing::debug!(
            event = "message.recreate",
            %session_id,
            %message_id,
            tenant_id,
            user_id,
            "webhook emitter (noop) — event swallowed",
        );
        Ok(())
    }

    async fn emit_message_aborted(
        &self,
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError> {
        tracing::debug!(
            event = "message.aborted",
            %session_id,
            %message_id,
            tenant_id,
            user_id,
            "webhook emitter (noop) — event swallowed",
        );
        Ok(())
    }

    async fn emit_session_deleted(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
        hard: bool,
    ) -> Result<(), ChatEngineError> {
        let event = if hard {
            WebhookEvent::SessionHardDeleted {
                session_id,
                tenant_id: tenant_id.into(),
                user_id: user_id.into(),
            }
        } else {
            WebhookEvent::SessionSoftDeleted {
                session_id,
                tenant_id: tenant_id.into(),
                user_id: user_id.into(),
            }
        };
        self.inner.emit(event).await
    }

    async fn emit_session_summary(
        &self,
        session_id: Uuid,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), ChatEngineError> {
        tracing::debug!(
            event = "session.summary",
            %session_id,
            tenant_id,
            user_id,
            "webhook emitter (noop) — event swallowed",
        );
        Ok(())
    }

    async fn emit_session_type_health_check(
        &self,
        session_type_id: Uuid,
    ) -> Result<(), ChatEngineError> {
        tracing::debug!(
            event = "session_type.health_check",
            %session_type_id,
            "webhook emitter (noop) — event swallowed",
        );
        Ok(())
    }
}

/// Bridge from the REST-layer [`WebhookEmitter`] to the legacy
/// domain-layer emitter via [`WebhookEmitterAdapter`]. Services that hold
/// an `Arc<dyn DomainWebhookEmitter>` can keep their old single-method
/// `emit(WebhookEvent)` signature; Phase 15 wraps any REST-layer emitter
/// in this adapter at module bootstrap.
pub struct WebhookEmitterAdapter<E: WebhookEmitter + ?Sized> {
    inner: std::sync::Arc<E>,
}

impl<E> WebhookEmitterAdapter<E>
where
    E: WebhookEmitter + ?Sized,
{
    #[must_use]
    pub fn new(inner: std::sync::Arc<E>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<E> DomainWebhookEmitter for WebhookEmitterAdapter<E>
where
    E: WebhookEmitter + ?Sized + Send + Sync,
{
    async fn emit(&self, event: WebhookEvent) -> Result<(), ChatEngineError> {
        match event {
            WebhookEvent::SessionCreated {
                session_id,
                tenant_id,
                user_id,
                session_type_id,
            } => {
                self.inner
                    .emit_session_created(session_id, &tenant_id, &user_id, session_type_id)
                    .await
            }
            WebhookEvent::SessionArchived {
                session_id,
                tenant_id,
                user_id,
            }
            | WebhookEvent::SessionRestored {
                session_id,
                tenant_id,
                user_id,
            } => {
                tracing::debug!(
                    %session_id,
                    tenant_id,
                    user_id,
                    "lifecycle event (archived/restored) — no dedicated webhook method",
                );
                Ok(())
            }
            WebhookEvent::SessionSoftDeleted {
                session_id,
                tenant_id,
                user_id,
            } => {
                self.inner
                    .emit_session_deleted(session_id, &tenant_id, &user_id, false)
                    .await
            }
            WebhookEvent::SessionHardDeleted {
                session_id,
                tenant_id,
                user_id,
            } => {
                self.inner
                    .emit_session_deleted(session_id, &tenant_id, &user_id, true)
                    .await
            }
            WebhookEvent::MessageDeleted {
                session_id,
                message_id,
                tenant_id,
                user_id,
                ..
            } => {
                self.inner
                    .emit_message_aborted(session_id, message_id, &tenant_id, &user_id)
                    .await
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::rest::dto::{StreamingChunkDto, StreamingEventDto, StreamingStartDto};
    use axum::body::to_bytes;
    use futures::stream;

    #[tokio::test]
    async fn ndjson_response_emits_application_x_ndjson() {
        let stream = stream::iter(vec![Ok::<_, ChatEngineError>(StreamingEventDto::Start(
            StreamingStartDto {
                message_id: Uuid::nil(),
            },
        ))]);

        let resp = ndjson_response(stream);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-ndjson"
        );
        assert_eq!(resp.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert_eq!(resp.headers().get("x-accel-buffering").unwrap(), "no");
    }

    #[tokio::test]
    async fn ndjson_response_writes_one_event_per_line() {
        let evts = vec![
            Ok::<_, ChatEngineError>(StreamingEventDto::Start(StreamingStartDto {
                message_id: Uuid::nil(),
            })),
            Ok::<_, ChatEngineError>(StreamingEventDto::Chunk(StreamingChunkDto {
                message_id: Uuid::nil(),
                chunk: "hello".into(),
            })),
        ];

        let resp = ndjson_response(stream::iter(evts));
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();

        let lines: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"type\":\"start\""));
        assert!(lines[1].contains("\"type\":\"chunk\""));
        assert!(lines[1].contains("\"chunk\":\"hello\""));
    }

    #[tokio::test]
    async fn ndjson_response_terminates_on_first_error() {
        let evts = vec![
            Ok::<_, ChatEngineError>(StreamingEventDto::Start(StreamingStartDto {
                message_id: Uuid::nil(),
            })),
            Err::<StreamingEventDto, _>(ChatEngineError::bad_request("oops")),
        ];

        let resp = ndjson_response(stream::iter(evts));
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\"type\":\"error\""));
    }

    #[tokio::test]
    async fn noop_webhook_emitter_satisfies_rest_trait() {
        let emitter: NoopWebhookEmitter = NoopWebhookEmitter::default();
        let via_rest: &dyn WebhookEmitter = &emitter;
        via_rest
            .emit_session_created(Uuid::nil(), "t", "u", None)
            .await
            .unwrap();
        via_rest
            .emit_session_type_health_check(Uuid::nil())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn webhook_emitter_adapter_routes_to_domain_trait() {
        let emitter = std::sync::Arc::new(NoopWebhookEmitter::default());
        let adapter = WebhookEmitterAdapter::new(emitter);
        let via_domain: &dyn DomainWebhookEmitter = &adapter;
        via_domain
            .emit(WebhookEvent::SessionCreated {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
                session_type_id: None,
            })
            .await
            .unwrap();
    }
}
