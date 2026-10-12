use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures::Stream;
use tokio::sync::mpsc;
use tokio::time::{Interval, interval};
use tokio_util::sync::CancellationToken;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use tracing::{Instrument, debug, info, warn};

use crate::api::rest::dto::{MessageDto, StreamMessageRequest};
use crate::api::rest::error::MiniChatChatError;
use crate::api::rest::sse::{StreamEventKind, StreamPhase};
use crate::domain::service::{StreamError, replay};
use crate::domain::stream_events::StreamEvent;
use crate::gear::AppServices;
use crate::infra::db::entity::chat_turn::Model as TurnModel;

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
    extract::Json(body): extract::Json<StreamMessageRequest>,
) -> Response {
    run_to_completion(async move { start_stream(&svc, ctx, chat_id, body).await }).await
}

/// Run a stream setup to its response in its own task. A client that
/// disconnects drops the handler future; without the task, a drop after the
/// turn (or the retry/edit mutation) commits would leave the new turn
/// `running`, with its reserve booked, until the orphan watchdog. With it the
/// setup finishes, the unsent response drops its `SseRelay`, and that
/// cancels the stream, which finalizes the turn as cancelled.
pub(crate) async fn run_to_completion(
    fut: impl std::future::Future<Output = Response> + Send + 'static,
) -> Response {
    match tokio::spawn(fut.instrument(tracing::Span::current())).await {
        Ok(response) => response,
        Err(e) => {
            tracing::error!(error = ?e, "stream setup task failed");
            CanonicalError::internal("stream setup failed")
                .create()
                .into_response()
        }
    }
}

#[allow(
    clippy::cognitive_complexity,
    reason = "sequential early returns with logging; tracing macros add most of the score"
)]
async fn start_stream(
    svc: &AppServices,
    ctx: SecurityContext,
    chat_id: uuid::Uuid,
    body: StreamMessageRequest,
) -> Response {
    // ── Pre-stream validation ──────────────────────────────────────────
    if body.content.trim().is_empty() {
        return MiniChatChatError::invalid_argument()
            .with_field_violation(
                "content",
                "Message content must not be empty",
                "EMPTY_CONTENT",
            )
            .create()
            .into_response();
    }

    // Resolve request_id early so it's available for error logging below.
    let request_id = body.request_id.unwrap_or_else(uuid::Uuid::new_v4);
    tracing::Span::current().record("turn_request_id", tracing::field::display(request_id));

    // ── Resolve model + provider from chat ─────────────────────────────
    let selected_model = match svc.chats.chat_model_for_send(&ctx, chat_id).await {
        Ok(model) => model,
        Err(e) => {
            warn!(error = %e, "failed to fetch chat for stream");
            return CanonicalError::from(e).into_response();
        }
    };

    let resolved = match svc
        .models
        .resolve_chat_model(ctx.subject_id(), &selected_model)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, model = %selected_model, "model resolution failed");
            return CanonicalError::from(e).into_response();
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
            return replay_response(svc, tenant_id, &selected_model, &turn, ping_secs).await;
        }
        Err(e) => return CanonicalError::from(e).into_response(),
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
    let scope = toolkit_security::AccessScope::for_tenant(tenant_id);

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
            let err = CanonicalError::internal("Failed to replay turn").create();
            return err.into_response();
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
