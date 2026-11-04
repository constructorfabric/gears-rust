//! Stream Supervisor (DESIGN §3.2, `cpt-cf-llm-gateway-component-stream-supervisor-v1`).
//!
//! Wraps the plugin event stream and guarantees what the consumer sees:
//!
//! - `response.created` and `response.in_progress` first, then the provider
//!   events, with `sequence_number` renumbered from 0 and the gateway
//!   response `id` forced on every snapshot;
//! - events relayed through a bounded channel (backpressure);
//! - exactly one terminal event: a provider terminal is relayed, a missing
//!   one is synthesized (`response.completed`), a provider `error` event or
//!   a failed structured-output validation becomes `response.failed`;
//!   everything after the terminal is dropped and a standalone `error` event
//!   is never emitted;
//! - dropping the returned stream cancels the request token and drops the
//!   plugin stream.
//!
//! Slice 3 scope: stream idle timeout / maximum duration, quota settle and
//! the incomplete-usage estimate belong to later slices.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::{
    ResponseError, ResponseStatus, ToolChoice, ToolChoiceMode, TruncationStrategy,
};
use llm_gateway_sdk::models::streaming::ResponseSnapshotEvent;
use llm_gateway_sdk::{
    CreateResponseBody, ResponseEventStream, ResponseResource, StreamingEvent, Usage,
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use super::output_validator::OutputValidator;

/// Last provider usage seen by the relay task, readable by the consumer side.
type SharedUsage = Arc<Mutex<Option<Usage>>>;

/// Inputs of one supervised stream.
#[domain_model]
pub struct StreamParams {
    /// Gateway response `id`, forced on every snapshot.
    pub response_id: String,
    /// In-progress snapshot carried by `response.created` and
    /// `response.in_progress` (see [`initial_snapshot`]).
    pub snapshot: ResponseResource,
    /// Capacity (events) of the bounded relay channel.
    pub channel_capacity: usize,
    /// Request cancellation token, cancelled when the consumer drops the
    /// stream (the plugin sees it through `ProviderCallCtx::cancel`).
    pub cancel: CancellationToken,
    /// Original `text.format` JSON schema: the accumulated output text is
    /// validated against it before the terminal event is emitted.
    pub output_schema: Option<serde_json::Value>,
    /// Gateway-side `ignored_parameters`, merged (sorted, deduplicated) with
    /// the provider's onto the terminal snapshot.
    pub ignored_parameters: Vec<String>,
    /// In-flight slot held for the lifetime of the stream.
    pub slot: Option<OwnedSemaphorePermit>,
}

/// The `in_progress` snapshot of response `response_id` for `body`.
#[must_use]
pub fn initial_snapshot(response_id: &str, body: &CreateResponseBody) -> ResponseResource {
    let created_at = now_secs();
    ResponseResource {
        id: response_id.to_owned(),
        object: "response".to_owned(),
        created_at,
        completed_at: None,
        status: ResponseStatus::InProgress,
        incomplete_details: None,
        model: body.model.clone(),
        instructions: body.instructions.clone(),
        output: Vec::new(),
        error: None,
        tools: body.tools.clone().unwrap_or_default(),
        tool_choice: body
            .tool_choice
            .clone()
            .unwrap_or(ToolChoice::Mode(ToolChoiceMode::Auto)),
        truncation: body.truncation.unwrap_or(TruncationStrategy::Disabled),
        parallel_tool_calls: body.parallel_tool_calls.unwrap_or(false),
        text: body.text.clone().unwrap_or_default(),
        top_p: body.top_p.unwrap_or(1.0),
        presence_penalty: body.presence_penalty.unwrap_or(0.0),
        frequency_penalty: body.frequency_penalty.unwrap_or(0.0),
        top_logprobs: body.top_logprobs.unwrap_or(0),
        temperature: body.temperature.unwrap_or(1.0),
        reasoning: body.reasoning.clone().unwrap_or_default(),
        usage: None,
        max_output_tokens: body.max_output_tokens,
        max_tool_calls: body.max_tool_calls,
        service_tier: "default".to_owned(),
        metadata: body.metadata.clone(),
        safety_identifier: body.safety_identifier.clone(),
        prompt_cache_key: body.prompt_cache_key.clone(),
        ignored_parameters: Vec::new(),
        provider_details: None,
    }
}

/// Supervise `upstream` (the plugin stream) and return the consumer-facing
/// stream.
///
/// A task drives the plugin stream into a bounded channel, so a slow consumer
/// stops the reads from the plugin. The task ends (dropping the plugin stream
/// and the in-flight slot) after the terminal event or when the consumer
/// drops the returned stream, which also cancels `params.cancel`.
///
/// Must be called within a Tokio runtime.
#[must_use]
pub fn supervise(upstream: ResponseEventStream, params: StreamParams) -> ResponseEventStream {
    let (tx, rx) = mpsc::channel(params.channel_capacity.max(1));
    let cancel = params.cancel.clone();
    let shared_usage = SharedUsage::default();
    let fallback = State::new(&params);
    tokio::spawn(relay(upstream, params, tx, Arc::clone(&shared_usage)));
    Box::pin(Relay {
        rx,
        cancel,
        fallback,
        shared_usage,
        received: 0,
        finished: false,
        ended: false,
    })
}

/// Consumer side of the relay channel. Dropping it before the terminal event
/// was delivered is a consumer disconnect and cancels the request.
///
/// If the relay task dies (panic, abort) before a terminal event, the stream
/// yields one gateway `response.failed` (`internal`) and cancels the request.
struct Relay {
    rx: mpsc::Receiver<StreamingEvent>,
    cancel: CancellationToken,
    /// Builds the `response.failed` for a relay task that died.
    fallback: State,
    /// Last provider usage the relay task saw (for the fallback terminal).
    shared_usage: SharedUsage,
    /// Events received so far: the next `sequence_number` (gapless from 0).
    received: u64,
    /// A terminal event was delivered, or the stream ended.
    finished: bool,
    /// The stream is over: every further poll yields `None`.
    ended: bool,
}

impl Stream for Relay {
    type Item = StreamingEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                self.received += 1;
                if is_terminal(&event) {
                    self.finished = true;
                }
                Poll::Ready(Some(event))
            }
            Poll::Ready(None) => {
                self.ended = true;
                if self.finished {
                    return Poll::Ready(None);
                }
                self.finished = true;
                self.cancel.cancel();
                tracing::error!(
                    response_id = %self.fallback.response_id,
                    "stream aborted: the relay task ended without a terminal event"
                );
                let last_usage = self
                    .shared_usage
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                self.fallback.last_usage = last_usage;
                let mut event = self.fallback.failed(ResponseError::new(
                    "internal",
                    "the response stream terminated unexpectedly",
                ));
                set_sequence(&mut event, self.received);
                Poll::Ready(Some(event))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel.cancel();
        }
    }
}

fn is_terminal(event: &StreamingEvent) -> bool {
    matches!(
        event,
        StreamingEvent::Completed(_) | StreamingEvent::Incomplete(_) | StreamingEvent::Failed(_)
    )
}

/// Overwrite the `sequence_number` of `event`.
fn set_sequence(event: &mut StreamingEvent, seq: u64) {
    match event {
        StreamingEvent::Created(e)
        | StreamingEvent::InProgress(e)
        | StreamingEvent::Queued(e)
        | StreamingEvent::Completed(e)
        | StreamingEvent::Incomplete(e)
        | StreamingEvent::Failed(e) => e.sequence_number = seq,
        StreamingEvent::OutputItemAdded(e) | StreamingEvent::OutputItemDone(e) => {
            e.sequence_number = seq;
        }
        StreamingEvent::ContentPartAdded(e) | StreamingEvent::ContentPartDone(e) => {
            e.sequence_number = seq;
        }
        StreamingEvent::OutputTextDelta(e) | StreamingEvent::OutputTextDone(e) => {
            e.sequence_number = seq;
        }
        StreamingEvent::OutputTextAnnotationAdded(e) => e.sequence_number = seq,
        StreamingEvent::RefusalDelta(e)
        | StreamingEvent::RefusalDone(e)
        | StreamingEvent::ReasoningDelta(e)
        | StreamingEvent::ReasoningDone(e) => e.sequence_number = seq,
        StreamingEvent::FunctionCallArgumentsDelta(e)
        | StreamingEvent::FunctionCallArgumentsDone(e) => e.sequence_number = seq,
        StreamingEvent::ReasoningSummaryPartAdded(e)
        | StreamingEvent::ReasoningSummaryPartDone(e) => e.sequence_number = seq,
        StreamingEvent::ReasoningSummaryTextDelta(e)
        | StreamingEvent::ReasoningSummaryTextDone(e) => e.sequence_number = seq,
        StreamingEvent::Error(e) => e.sequence_number = seq,
        StreamingEvent::DataInProgress(e) | StreamingEvent::DataDone(e) => {
            e.sequence_number = seq;
        }
        StreamingEvent::Other(extension) => {
            if let Some(object) = extension.0.as_object_mut() {
                object.insert("sequence_number".to_owned(), seq.into());
            }
        }
    }
}

/// Per-stream relay state.
struct State {
    response_id: String,
    snapshot: ResponseResource,
    ignored_parameters: Vec<String>,
    output_schema: Option<serde_json::Value>,
    /// Concatenated `response.output_text.delta` text (schema streams only);
    /// `None` until the first delta.
    text: Option<String>,
    /// Last usage a provider snapshot carried.
    last_usage: Option<Usage>,
    next_sequence: u64,
}

impl State {
    fn new(params: &StreamParams) -> Self {
        Self {
            response_id: params.response_id.clone(),
            snapshot: params.snapshot.clone(),
            ignored_parameters: params.ignored_parameters.clone(),
            output_schema: params.output_schema.clone(),
            text: None,
            last_usage: None,
            next_sequence: 0,
        }
    }

    /// Number `event` and send it; `false` when the consumer is gone.
    async fn emit(&mut self, tx: &mpsc::Sender<StreamingEvent>, mut event: StreamingEvent) -> bool {
        set_sequence(&mut event, self.next_sequence);
        self.next_sequence += 1;
        tx.send(event).await.is_ok()
    }

    /// Usage of a terminal the provider did not finish: the last usage seen,
    /// marked incomplete (estimation belongs to the Usage Reporter).
    fn interrupted_usage(&self) -> Usage {
        self.last_usage
            .clone()
            .map_or_else(Usage::unknown, |mut usage| {
                usage.complete = false;
                usage.estimated = false;
                usage
            })
    }

    /// Gateway-owned fields of a terminal snapshot: id and merged
    /// `ignored_parameters`.
    fn finish_snapshot(&self, response: &mut ResponseResource) {
        self.response_id.clone_into(&mut response.id);
        let mut ignored = std::mem::take(&mut response.ignored_parameters);
        ignored.extend(self.ignored_parameters.iter().cloned());
        ignored.sort();
        ignored.dedup();
        response.ignored_parameters = ignored;
        // Every terminal carries one Usage; provider-supplied usage is kept.
        if response.usage.is_none() {
            response.usage = Some(self.interrupted_usage());
        }
    }

    /// A terminal built by the gateway on top of the initial snapshot.
    fn own_terminal(
        &self,
        status: ResponseStatus,
        error: Option<ResponseError>,
    ) -> ResponseResource {
        let mut response = self.snapshot.clone();
        response.status = status;
        response.error = error;
        response.completed_at = Some(now_secs());
        response.usage = Some(self.interrupted_usage());
        self.finish_snapshot(&mut response);
        response
    }

    fn failed(&self, error: ResponseError) -> StreamingEvent {
        snapshot_event(
            StreamingEvent::Failed,
            self.own_terminal(ResponseStatus::Failed, Some(error)),
        )
    }

    /// Map one provider event to the consumer event; `None` drops it
    /// (provider lifecycle snapshots are replaced by the gateway's own).
    fn translate(&mut self, event: StreamingEvent) -> Option<StreamingEvent> {
        Some(match event {
            StreamingEvent::Created(e)
            | StreamingEvent::InProgress(e)
            | StreamingEvent::Queued(e) => {
                self.last_usage = e.response.usage.or(self.last_usage.take());
                return None;
            }
            StreamingEvent::Error(e) => {
                tracing::warn!(
                    response_id = %self.response_id,
                    code = %e.error.code,
                    "stream aborted: the provider sent an error event"
                );
                self.failed(e.error)
            }
            StreamingEvent::Completed(e) => self.completed(e.response),
            StreamingEvent::Incomplete(e) => {
                let mut response = e.response;
                self.finish_snapshot(&mut response);
                snapshot_event(StreamingEvent::Incomplete, response)
            }
            StreamingEvent::Failed(e) => {
                let mut response = e.response;
                self.finish_snapshot(&mut response);
                snapshot_event(StreamingEvent::Failed, response)
            }
            StreamingEvent::OutputTextDelta(e) => {
                if self.output_schema.is_some() {
                    self.text.get_or_insert_with(String::new).push_str(&e.text);
                }
                StreamingEvent::OutputTextDelta(e)
            }
            other => other,
        })
    }

    /// The provider reported `Completed`: validate structured output, then
    /// relay it (or fail the response).
    fn completed(&self, mut response: ResponseResource) -> StreamingEvent {
        if let Some(schema) = &self.output_schema {
            let verdict = match self.text.as_deref() {
                Some(text) => OutputValidator.validate_text(schema, Some(text)),
                None => OutputValidator.validate(schema, &response),
            };
            if let Err(err) = verdict {
                tracing::warn!(
                    response_id = %self.response_id,
                    code = err.code(),
                    "stream aborted: structured output validation failed"
                );
                let mut failed =
                    self.own_terminal(ResponseStatus::Failed, Some(err.to_response_error()));
                // Keep the provider's usage: the tokens were spent.
                if response.usage.is_some() {
                    failed.usage = response.usage.take();
                }
                return snapshot_event(StreamingEvent::Failed, failed);
            }
        }
        self.finish_snapshot(&mut response);
        snapshot_event(StreamingEvent::Completed, response)
    }
}

fn snapshot_event(
    wrap: fn(ResponseSnapshotEvent) -> StreamingEvent,
    response: ResponseResource,
) -> StreamingEvent {
    wrap(ResponseSnapshotEvent {
        sequence_number: 0,
        response,
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Drive `upstream` into `tx` until a terminal event was sent or the consumer
/// left.
async fn relay(
    mut upstream: ResponseEventStream,
    params: StreamParams,
    tx: mpsc::Sender<StreamingEvent>,
    shared_usage: SharedUsage,
) {
    let mut state = State::new(&params);
    // Released when the task ends, whichever way.
    let _slot = params.slot;
    let created = snapshot_event(StreamingEvent::Created, state.snapshot.clone());
    let in_progress = snapshot_event(StreamingEvent::InProgress, state.snapshot.clone());
    if !state.emit(&tx, created).await || !state.emit(&tx, in_progress).await {
        log_disconnect(&state.response_id);
        return;
    }

    loop {
        let next = tokio::select! {
            () = tx.closed() => {
                log_disconnect(&state.response_id);
                return;
            }
            next = upstream.next() => next,
        };
        let Some(event) = next else {
            tracing::warn!(
                response_id = %state.response_id,
                "stream aborted: the provider stream ended without a terminal event"
            );
            let response = state.own_terminal(ResponseStatus::Completed, None);
            let terminal = state.completed(response);
            let _ = state.emit(&tx, terminal).await;
            return;
        };
        let Some(outgoing) = state.translate(event) else {
            // Dropped lifecycle snapshots are the only source of `last_usage`.
            shared_usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone_from(&state.last_usage);
            continue;
        };
        let terminal = is_terminal(&outgoing);
        if !state.emit(&tx, outgoing).await {
            log_disconnect(&state.response_id);
            return;
        }
        if terminal {
            return;
        }
    }
}

fn log_disconnect(response_id: &str) {
    tracing::info!(response_id, "stream aborted: the consumer disconnected");
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "stream_supervisor_tests.rs"]
mod stream_supervisor_tests;
