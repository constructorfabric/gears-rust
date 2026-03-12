use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::Path;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures::Stream;
use modkit::api::odata::OData;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;
use tokio::sync::mpsc;
use tokio::time::{Interval, interval};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, warn};

use crate::api::rest::dto::{MessageDto, StreamMessageRequest};
use crate::api::rest::sse::{StreamEventKind, StreamPhase};
use crate::domain::service::{StreamAttachmentContext, StreamError, replay};
use crate::domain::stream_events::StreamEvent;
use crate::infra::db::entity::chat_turn::Model as TurnModel;
use crate::module::AppServices;

/// GET /mini-chat/v1/chats/{id}/messages
#[tracing::instrument(skip(svc, ctx, query), fields(chat_id = %chat_id))]
pub(crate) async fn list_messages(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(chat_id): Path<uuid::Uuid>,
    OData(query): OData,
) -> ApiResult<JsonPage<MessageDto>> {
    let page = svc.messages.list_messages(&ctx, chat_id, &query).await?;
    let page = page.map_items(MessageDto::from);
    Ok(Json(page))
}

/// POST /mini-chat/v1/chats/{id}/messages:stream
///
/// Pre-stream validation returns JSON errors. On success, opens an SSE
/// connection and relays events from the provider through a bounded channel.
#[tracing::instrument(skip(svc, ctx, body), fields(chat_id = %chat_id, turn_request_id))]
pub(crate) async fn stream_message(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(chat_id): Path<uuid::Uuid>,
    Json(body): Json<StreamMessageRequest>,
) -> Response {
    // ── Pre-stream validation ──────────────────────────────────────────
    if body.content.trim().is_empty() {
        return Problem::new(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "Message content must not be empty",
        )
        .into_response();
    }

    // Resolve request_id early so it's available for error logging below.
    let request_id = body.request_id.unwrap_or_else(uuid::Uuid::new_v4);
    tracing::Span::current().record("turn_request_id", tracing::field::display(request_id));

    // ── Resolve model + provider from chat ─────────────────────────────
    let chat = match svc.chats.get_chat(&ctx, chat_id).await {
        Ok(c) => c,
        Err(e) => {
            let (status, detail) = if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                warn!(error = %e, "failed to fetch chat for stream");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal error occurred".to_owned(),
                )
            };
            return Problem::new(status, "Error", detail).into_response();
        }
    };

    let selected_model = chat.model;
    let resolved = match svc
        .models
        .resolve_model(ctx.subject_id(), Some(selected_model.clone()))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, model = %selected_model, "model resolution failed");
            return Problem::new(StatusCode::BAD_REQUEST, "Bad Request", e.to_string())
                .into_response();
        }
    };

    // ── Validate attachments & build retrieval scope ────────────────────
    let attachment_ctx = match build_attachment_context(
        &svc,
        &ctx,
        chat_id,
        &body.attachment_ids,
        &body.rag_attachment_ids,
    )
    .await
    {
        Ok(ac) => ac,
        Err(e) => {
            return Problem::from(e).into_response();
        }
    };

    // ── Wire up streaming pipeline ─────────────────────────────────────
    let capacity = svc.stream.channel_capacity();
    let ping_secs = svc.stream.ping_interval_secs();
    let (tx, rx) = mpsc::channel::<StreamEvent>(capacity);
    let cancel = CancellationToken::new();

    info!(model = %resolved.model_id, provider_id = %resolved.provider_id, "starting SSE stream");

    // Pre-stream checks + spawn the provider task
    let provider_handle = match svc
        .stream
        .run_stream(
            ctx,
            chat_id,
            request_id,
            body.content,
            resolved,
            attachment_ctx,
            cancel.clone(),
            tx,
        )
        .await
    {
        Ok(handle) => handle,
        Err(StreamError::Replay { turn }) => {
            return replay_response(&svc, &selected_model, &turn, ping_secs).await;
        }
        Err(e) => return stream_error_response(&e),
    };

    // Monitor provider task for panics
    let monitor_span = tracing::Span::current();
    tokio::spawn(
        async move {
            if let Err(e) = provider_handle.await {
                tracing::error!(error = ?e, "provider task panicked");
            }
        }
        .instrument(monitor_span),
    );

    // Build the SSE relay stream
    let relay = SseRelay::new(rx, cancel, ping_secs);

    Sse::new(relay)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

/// Build the attachment context for the streaming pipeline.
///
/// Validates `attachment_ids` + `rag_attachment_ids`, resolves the retrieval scope,
/// and builds the tools/image inputs for the provider request.
async fn build_attachment_context(
    svc: &AppServices,
    ctx: &SecurityContext,
    chat_id: uuid::Uuid,
    attachment_ids: &[uuid::Uuid],
    rag_attachment_ids: &[uuid::Uuid],
) -> Result<StreamAttachmentContext, crate::domain::error::DomainError> {
    use crate::domain::service::retrieval_scope::{self, AttachmentKind, RetrievalScope};
    use StreamAttachmentContext;
    use mini_chat_sdk::models::AttachmentKind as SdkAttachmentKind;

    // If no attachments at all, return default (no tools, no images)
    if attachment_ids.is_empty() && rag_attachment_ids.is_empty() {
        // Still check if chat has documents for implicit AllChat scope
        let validated = svc
            .attachments
            .validate_send_message_attachments(ctx, chat_id, &[], &[])
            .await?;

        if !validated.chat_has_ready_documents {
            return Ok(StreamAttachmentContext::default());
        }

        // Chat has documents but no explicit scope — get vector store for AllChat
        let vs = svc
            .attachments
            .get_chat_vector_store_id(ctx, chat_id)
            .await?;
        return Ok(match vs {
            Some(vs_id) => StreamAttachmentContext {
                tools: vec![crate::infra::llm::LlmTool::FileSearch {
                    vector_store_ids: vec![vs_id],
                }],
                file_search_enabled: true,
                ..Default::default()
            },
            None => StreamAttachmentContext::default(),
        });
    }

    let validated = svc
        .attachments
        .validate_send_message_attachments(ctx, chat_id, attachment_ids, rag_attachment_ids)
        .await?;

    // Build attachment kind map for retrieval scope resolution
    let kind_map: std::collections::HashMap<uuid::Uuid, AttachmentKind> = validated
        .message_attachments
        .iter()
        .map(|a| {
            let kind = match a.attachment.kind {
                SdkAttachmentKind::Image => AttachmentKind::Image,
                SdkAttachmentKind::Document => AttachmentKind::Document,
            };
            (a.attachment.id, kind)
        })
        .collect();

    let scope = retrieval_scope::resolve_retrieval_scope(
        rag_attachment_ids,
        attachment_ids,
        &kind_map,
        validated.chat_has_ready_documents,
    );

    // Resolve image file IDs from attachment_ids
    let image_file_ids: Vec<String> = validated
        .message_attachments
        .iter()
        .filter(|a| a.attachment.kind == SdkAttachmentKind::Image)
        .map(|a| a.provider_file_id.clone())
        .collect();
    let num_images = u32::try_from(image_file_ids.len()).unwrap_or(u32::MAX);

    // Build tools based on retrieval scope
    let (tools, file_search_enabled) = match &scope {
        RetrievalScope::None => (vec![], false),
        RetrievalScope::AllChat | RetrievalScope::Scoped(_) => {
            match svc
                .attachments
                .get_chat_vector_store_id(ctx, chat_id)
                .await?
            {
                Some(vs_id) => {
                    // TODO: add metadata filtering for Scoped(ids) when provider supports it
                    (
                        vec![crate::infra::llm::LlmTool::FileSearch {
                            vector_store_ids: vec![vs_id],
                        }],
                        true,
                    )
                }
                None => (vec![], false),
            }
        }
    };

    Ok(StreamAttachmentContext {
        tools,
        image_file_ids,
        file_search_enabled,
        num_images,
    })
}

/// Map a [`StreamError`] to an appropriate HTTP error response.
///
/// Caller is expected to be within an instrumented span that carries
/// `chat_id` and `turn_request_id` fields.
#[allow(clippy::cognitive_complexity)]
fn stream_error_response(err: &StreamError) -> Response {
    match err {
        StreamError::Replay { .. } => {
            // Completed turns are handled by replay_response(); this arm covers
            // the defensive case where Replay leaks through without interception.
            Problem::new(StatusCode::CONFLICT, "Conflict", "Duplicate request_id").into_response()
        }
        StreamError::Conflict { message, code } => {
            info!(conflict_code = %code, "stream request conflict");
            Problem::new(StatusCode::CONFLICT, "Conflict", message).into_response()
        }
        StreamError::ChatNotFound { .. } => {
            Problem::new(StatusCode::NOT_FOUND, "Not Found", "Chat not found").into_response()
        }
        StreamError::AuthorizationFailed { source } => {
            warn!(error = %source, "stream authorization failed");
            Problem::new(StatusCode::FORBIDDEN, "Forbidden", "Access denied").into_response()
        }
        StreamError::TurnCreationFailed { source } => {
            warn!(error = %source, "pre-stream turn creation failed");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                "Failed to initialize turn",
            )
            .into_response()
        }
        StreamError::QuotaExhausted {
            error_code,
            http_status,
        } => {
            info!(error_code = %error_code, http_status = *http_status, "quota exhausted, request rejected");
            let status =
                StatusCode::from_u16(*http_status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
            Problem::new(status, "Quota Exhausted", error_code).into_response()
        }
    }
}

/// Build an SSE replay response for a completed turn.
///
/// Fetches stored assistant content and emits `delta` + `done` events through
/// the same `SseRelay` infrastructure as normal streaming.
async fn replay_response(
    svc: &AppServices,
    selected_model: &str,
    turn: &TurnModel,
    ping_secs: u64,
) -> Response {
    let scope = modkit_security::AccessScope::allow_all().tenant_only();

    let events = match replay::replay_turn(
        &svc.db,
        &*svc.message_repo,
        &scope,
        turn,
        selected_model,
    )
    .await
    {
        Ok(ev) => ev,
        Err(e) => {
            warn!(error = %e, turn_id = %turn.id, "replay failed");
            return Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                "Failed to replay turn",
            )
            .into_response();
        }
    };

    let (tx, rx) = mpsc::channel::<StreamEvent>(4);
    tokio::spawn(async move {
        drop(tx.send(events.delta).await);
        drop(tx.send(events.done).await);
    });

    let cancel = CancellationToken::new();
    let relay = SseRelay::new(rx, cancel, ping_secs);

    Sse::new(relay)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// SseRelay — handler-side relay loop as a Stream
// ════════════════════════════════════════════════════════════════════════════

/// SSE relay that reads from the provider channel, enforces event ordering,
/// emits ping keepalives, and respects cancellation.
///
/// Implements `Stream<Item = Result<Event, Infallible>>` for Axum SSE.
pub(crate) struct SseRelay {
    rx: mpsc::Receiver<StreamEvent>,
    cancel: CancellationToken,
    phase: StreamPhase,
    ping_timer: Interval,
    done: bool,
    /// TODO: will be used for disconnect-stage reporting
    first_delta_emitted: bool,
}

impl SseRelay {
    pub(crate) fn new(
        rx: mpsc::Receiver<StreamEvent>,
        cancel: CancellationToken,
        ping_secs: u64,
    ) -> Self {
        Self {
            rx,
            cancel,
            phase: StreamPhase::Idle,
            ping_timer: interval(Duration::from_secs(ping_secs)),
            done: false,
            first_delta_emitted: false,
        }
    }
}

impl Drop for SseRelay {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Stream for SseRelay {
    type Item = Result<Event, Infallible>;

    #[allow(clippy::cognitive_complexity)]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.done {
            return Poll::Ready(None);
        }

        // Check cancellation
        if this.cancel.is_cancelled() {
            this.done = true;
            return Poll::Ready(None);
        }

        // Try to receive an event from the channel (non-blocking poll)
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                let kind = event.event_kind();
                let is_terminal = event.is_terminal();

                // Enforce ordering via StreamPhase
                match this.phase.try_advance(kind) {
                    Ok(new_phase) => {
                        this.phase = new_phase;
                    }
                    Err(violation) => {
                        warn!(%violation, "suppressing out-of-order SSE event");
                        // Wake immediately to try next event
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }

                // Track first delta for disconnect stage reporting
                if kind == StreamEventKind::Delta {
                    this.first_delta_emitted = true;
                }

                // Convert to SSE Event
                let sse_event = match event.into_sse_event() {
                    Ok(e) => e,
                    Err(e) => {
                        warn!(error = %e, "failed to serialize SSE event");
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                };

                // Terminal events end the stream
                if is_terminal {
                    this.done = true;
                }

                // Reset ping timer on any event
                this.ping_timer.reset();

                Poll::Ready(Some(Ok(sse_event)))
            }
            Poll::Ready(None) => {
                // Channel closed — provider task exited
                this.done = true;

                // If no terminal event was received, emit an error to honour
                // the SSE contract (streams must end with done or error).
                if this.phase.is_terminal() {
                    debug!("provider channel closed");
                } else {
                    warn!(
                        "provider channel closed without terminal event - emitting synthetic error"
                    );
                    let error_event = StreamEvent::Error(crate::domain::stream_events::ErrorData {
                        code: "stream_interrupted".to_owned(),
                        message: "Provider stream ended unexpectedly".to_owned(),
                    });
                    if let Ok(sse) = error_event.into_sse_event() {
                        return Poll::Ready(Some(Ok(sse)));
                    }
                }

                Poll::Ready(None)
            }
            Poll::Pending => {
                // No event ready — check if ping timer fired
                if this.ping_timer.poll_tick(cx).is_ready() {
                    // Only emit pings in Idle or Pinging phase
                    let kind = StreamEventKind::Ping;
                    match this.phase.try_advance(kind) {
                        Ok(new_phase) => {
                            this.phase = new_phase;
                            #[allow(clippy::expect_used)]
                            let ping = StreamEvent::Ping
                                .into_sse_event()
                                .expect("ping serialization cannot fail");
                            Poll::Ready(Some(Ok(ping)))
                        }
                        Err(_) => {
                            // Past pinging phase — skip the ping silently
                            Poll::Pending
                        }
                    }
                } else {
                    Poll::Pending
                }
            }
        }
    }
}
