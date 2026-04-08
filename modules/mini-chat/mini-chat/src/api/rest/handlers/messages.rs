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
use crate::domain::service::{StreamError, replay};
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

    // ── Extract web search flag from DTO ───────────────────────────────
    let web_search_enabled = body.web_search.as_ref().is_some_and(|c| c.enabled);

    // ── Wire up streaming pipeline ─────────────────────────────────────
    let capacity = svc.stream.channel_capacity();
    let ping_secs = svc.stream.ping_interval_secs();
    let (tx, rx) = mpsc::channel::<StreamEvent>(capacity);
    let cancel = CancellationToken::new();

    // Capture tenant_id before `ctx` is moved into `run_stream`.
    let tenant_id = ctx.subject_tenant_id();

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
            web_search_enabled,
            body.attachment_ids,
            cancel.clone(),
            tx,
        )
        .await
    {
        Ok(handle) => handle,
        Err(StreamError::Replay { turn }) => {
            return replay_response(&svc, tenant_id, &selected_model, &turn, ping_secs).await;
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
            quota_scope,
        } => {
            info!(error_code = %error_code, http_status = *http_status, quota_scope = %quota_scope, "quota exhausted, request rejected");
            let status =
                StatusCode::from_u16(*http_status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
            // TODO(P2): include `quota_scope` in the response body so clients can
            // distinguish token vs web_search quota exhaustion (DESIGN.md §5.2).
            Problem::new(status, error_code, error_code).into_response()
        }
        StreamError::WebSearchDisabled => {
            info!(
                reason = "kill_switch",
                "web search disabled via kill switch, request rejected"
            );
            Problem::new(
                StatusCode::BAD_REQUEST,
                "web_search_disabled",
                "Web search is currently disabled",
            )
            .into_response()
        }
        StreamError::ImagesDisabled => {
            info!(
                reason = "kill_switch",
                "images disabled via kill switch, request rejected"
            );
            Problem::new(
                StatusCode::BAD_REQUEST,
                "images_disabled",
                "Image inputs are currently disabled",
            )
            .into_response()
        }
        StreamError::TooManyImages { count, max } => {
            info!(count, max, "too many image attachments in request");
            Problem::new(
                StatusCode::BAD_REQUEST,
                "too_many_images",
                format!("Request includes {count} images, maximum is {max}"),
            )
            .into_response()
        }
        StreamError::UnsupportedMedia => {
            info!("model does not support image input, request rejected");
            Problem::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media",
                "The selected model does not support image input",
            )
            .into_response()
        }
        StreamError::InvalidAttachment { code, message } => {
            info!(code = %code, message = %message, "invalid attachment in request");
            Problem::new(StatusCode::BAD_REQUEST, code, message).into_response()
        }
        StreamError::ContextBudgetExceeded {
            required_tokens,
            available_tokens,
        } => {
            info!(
                required_tokens,
                available_tokens, "context budget exceeded, request rejected"
            );
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "context_budget_exceeded",
                format!(
                    "Context requires {required_tokens} tokens but only {available_tokens} are available"
                ),
            )
            .into_response()
        }
    }
}

/// Build an SSE replay response for a completed turn.
///
/// Fetches stored assistant content and emits `delta` + `done` events through
/// the same `SseRelay` infrastructure as normal streaming.
async fn replay_response(
    svc: &AppServices,
    tenant_id: uuid::Uuid,
    selected_model: &str,
    turn: &TurnModel,
    ping_secs: u64,
) -> Response {
    let scope = modkit_security::AccessScope::for_tenant(tenant_id);

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
        drop(tx.send(events.stream_started).await);
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
                    // Only emit pings in Started or Pinging phase
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
