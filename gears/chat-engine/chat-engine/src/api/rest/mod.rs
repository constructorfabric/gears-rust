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
//! - [`mod@self`] — SSE delta streaming helper + `WebhookEmitter` trait.
//!
//! ## SSE delta streaming
//!
//! The streaming endpoints (`POST messages`, `POST messages/recreate`,
//! `POST sessions/{id}/summarize`, `GET messages/{id}/stream`) emit the
//! `start`/`delta`/`complete`/`error` delta protocol (FR-024) over
//! `text/event-stream`. See [`sse_delta_stream_response`] (live projection)
//! and [`stream_reader::sse_buffer_reader_response`] (resume reader) for the
//! response builders.
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
pub(crate) mod stream_reader;

pub use routes::register_routes;

use std::convert::Infallible;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use futures::stream::StreamExt;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::service::webhook::{
    NoopWebhookEmitter as DomainNoopWebhookEmitter, WebhookEmitter as DomainWebhookEmitter,
    WebhookEvent,
};

// ===========================================================================
// SSE delta streaming helper
// ===========================================================================

/// Build a `text/event-stream` (Server-Sent Events) **delta** response from an
/// infallible `StreamingEvent` stream — the shape returned by the message /
/// variant services (`SendMessageStream`). The plugin's
/// `Start`/`Chunk`/`Complete`/`Error` events are projected by
/// [`DeltaProjector`](crate::domain::stream_delta::DeltaProjector) into the
/// client-facing `start`/`delta`/`complete`/`error` protocol (FR-024); each
/// wire event is emitted as one SSE frame (`id:` = `seq`, `event:` = type,
/// `data:` = JSON). Mid-stream errors travel as an `error` event, never a
/// `Result::Err`.
///
/// Under true live-tail (FR-024) this response does **not** own a
/// cancellation guard: dropping the body (client disconnect) stops delivery
/// but the detached driver keeps generating and buffering, so a reconnect via
/// `Last-Event-ID` resumes the stream.
pub(crate) fn sse_delta_stream_response(
    stream: crate::domain::service::message_service::SendMessageStream,
) -> Response {
    use crate::domain::stream_delta::DeltaProjector;
    use futures::stream;

    // True live-tail (FR-024): a client disconnect does NOT cancel the driver.
    // The driver runs to completion, buffering every event, so a reconnect via
    // `Last-Event-ID` resumes seamlessly. Hence no `drop_guard` here — dropping
    // this response body just stops *delivery*, not *generation*.
    //
    // The projector below is independent of the driver's buffer projector but
    // sees the identical `StreamingEvent` sequence, so the `seq` it stamps on
    // the wire matches the buffer — the client's `Last-Event-ID` lines up on
    // reconnect.
    let wire = stream
        .scan(DeltaProjector::new(), |proj, evt| {
            std::future::ready(Some(stream::iter(proj.project(evt))))
        })
        .flatten();

    let body_stream = wire.map(|w| std::result::Result::<_, Infallible>::Ok(sse_frame(&w)));

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .header("x-accel-buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "failed to build SSE stream response");
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

/// Serialize one wire delta event into an SSE frame:
/// `id: <seq>\nevent: <type>\ndata: <json>\n\n`.
fn sse_frame(evt: &crate::domain::stream_delta::WireStreamEvent) -> Vec<u8> {
    let data = serde_json::to_string(evt).unwrap_or_else(|err| {
        tracing::error!(error = %err, "failed to serialize wire delta event");
        r#"{"type":"message.error","error":"internal serialization failure"}"#.to_string()
    });
    format!(
        "id: {}\nevent: {}\ndata: {}\n\n",
        evt.seq(),
        evt.event_name(),
        data
    )
    .into_bytes()
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
            "webhook emitter (noop) \u{2014} event swallowed",
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
            "webhook emitter (noop) \u{2014} event swallowed",
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
            "webhook emitter (noop) \u{2014} event swallowed",
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
            "webhook emitter (noop) \u{2014} event swallowed",
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
            "webhook emitter (noop) \u{2014} event swallowed",
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
                    "lifecycle event (archived/restored) \u{2014} no dedicated webhook method",
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
