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
//! At every terminal path (provider or synthesized terminal, provider error,
//! consumer disconnect, relay failure) the [`StreamParams::on_terminal`] hook
//! is called exactly once with the terminal usage (the Quota Guard settle).
//! Final provider usage is passed through; without it the core estimate of
//! [`StreamParams::estimator`] applies (see [`UsageBasis`]).
//!
//! [`StreamParams::limits`] bounds the stream: no provider event within the
//! idle timeout, the maximum duration from the stream start, and the shutdown
//! cut (see [`StreamLimits`]).

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::{
    ResponseError, ResponseStatus, ToolChoice, ToolChoiceMode, TruncationStrategy,
};
use llm_gateway_sdk::models::streaming::ResponseSnapshotEvent;
use llm_gateway_sdk::{
    CreateResponseBody, ResponseEventStream, ResponseResource, StreamingEvent, Usage,
};
use rust_decimal::Decimal;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use tracing::Instrument as _;

use super::output_validator::OutputValidator;
use super::pricing::UsageEstimator;

/// Called exactly once per stream, at its terminal state, with the terminal
/// usage: `cost` is the AI credit quantity, `complete` / `estimated` follow
/// the DESIGN usage table. Every terminal path calls it: a provider or
/// synthesized terminal, a provider error, consumer disconnect and relay
/// failure. The second argument is the provider request id carried by the
/// terminal snapshot's provider details (`None` when the stream ended
/// without a provider terminal or the plugin captured none).
pub type TerminalHook = Box<dyn FnOnce(Usage, Option<String>) + Send + 'static>;

/// What the relay saw so far; shared by the relay task, its terminal guard and
/// the consumer side (which builds the terminal when the task dies).
#[derive(Default)]
struct Progress {
    /// Last usage a provider snapshot carried (partial, not final).
    last_usage: Option<Usage>,
    /// UTF-8 bytes of output (text, refusal, reasoning, tool arguments)
    /// relayed to the consumer.
    output_bytes: u64,
}

/// The facts the terminal usage is derived from when the provider gave no
/// final usage (DESIGN §3.2 Usage Reporter table).
#[derive(Clone)]
struct UsageBasis {
    progress: Arc<Mutex<Progress>>,
    estimator: Option<UsageEstimator>,
}

impl UsageBasis {
    fn new(estimator: Option<UsageEstimator>) -> Self {
        Self {
            progress: Arc::default(),
            estimator,
        }
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn note_usage(&self, usage: Usage) {
        self.progress().last_usage = Some(usage);
    }

    fn note_output(&self, bytes: usize) {
        let mut progress = self.progress();
        progress.output_bytes = progress.output_bytes.saturating_add(bytes as u64);
    }

    /// Usage when the provider reported no final usage: the core estimate
    /// (`complete = false`, `estimated = true`); without an estimator the last
    /// partial usage numbers at zero cost, `complete = false`,
    /// `estimated = false` (no estimate can be made).
    fn no_final(&self) -> Usage {
        let progress = self.progress();
        match &self.estimator {
            Some(estimator) => {
                estimator.estimate(progress.output_bytes, progress.last_usage.as_ref())
            }
            None => progress
                .last_usage
                .clone()
                .map_or_else(Usage::unknown, |mut usage| {
                    usage.complete = false;
                    usage.estimated = false;
                    usage.cost = Decimal::ZERO;
                    usage
                }),
        }
    }

    /// Usage of a provider error without usage: the estimate when output was
    /// already relayed to the consumer, otherwise a final zero.
    fn provider_error(&self) -> Usage {
        if self.estimator.is_some() && self.progress().output_bytes == 0 {
            Usage::reported(0, 0, Decimal::ZERO)
        } else {
            self.no_final()
        }
    }
}

/// Calls the terminal hook exactly once: with the sent terminal's usage via
/// [`TerminalGuard::finish`], or, when the relay ends any other way (consumer
/// disconnect, task death), with the no-final-usage estimate on drop.
struct TerminalGuard {
    basis: UsageBasis,
    hook: Option<TerminalHook>,
}

impl TerminalGuard {
    fn finish(&mut self, usage: Usage, provider_request_id: Option<String>) {
        if let Some(hook) = self.hook.take() {
            hook(usage, provider_request_id);
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(hook) = self.hook.take() {
            hook(self.basis.no_final(), None);
        }
    }
}

/// Which fallback usage a terminal without usage gets.
#[derive(Clone, Copy)]
enum MissingUsage {
    /// No final provider usage: the core estimate.
    NoFinal,
    /// A provider error: the estimate only if output was relayed.
    ProviderError,
}

/// Time and shutdown bounds of one supervised stream. The default bounds
/// nothing.
#[domain_model]
#[derive(Clone, Default)]
pub struct StreamLimits {
    /// `timeouts.stream_idle`: longest wait for the next provider event. Only
    /// time spent waiting on the plugin stream counts: a consumer that is not
    /// reading (backpressure) does not make the provider idle. Expiry ends the
    /// stream with `response.failed` (`provider_unavailable`).
    pub idle_timeout: Option<Duration>,
    /// `timeouts.stream_max_duration`: longest total duration, from the start
    /// of the supervised stream (consumer time included). Expiry ends the
    /// stream like the idle timeout.
    pub max_duration: Option<Duration>,
    /// Fires at the shutdown drain deadline: the stream ends with
    /// `response.failed` (`gateway_unavailable`).
    pub shutdown_cut: Option<CancellationToken>,
}

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
    /// Estimate for terminals without final provider usage. `None`: no
    /// estimate can be made (the last seen numbers at zero cost, flags false).
    pub estimator: Option<UsageEstimator>,
    /// Terminal hook (the Quota Guard settle); see [`TerminalHook`].
    pub on_terminal: Option<TerminalHook>,
    /// Idle / maximum duration / shutdown bounds.
    pub limits: StreamLimits,
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
    let basis = UsageBasis::new(params.estimator);
    let fallback = State::new(&params, basis.clone());
    let stop = StopCell::default();
    tokio::spawn(relay(upstream, params, tx, basis, stop.clone()).in_current_span());
    Box::pin(Relay {
        rx,
        cancel,
        stop,
        fallback,
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
    /// Set by the relay when a bound ended the stream: the consumer then gets
    /// the `response.failed` at its next read, ahead of events still buffered.
    stop: StopCell,
    /// Builds the `response.failed` for a relay task that died.
    fallback: State,
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
        if let Some(stop) = self.stop.take() {
            // A bound ended the stream (the hook already settled with this
            // usage): the terminal goes out at once, so a consumer that reads
            // slowly is not held up by the events still buffered.
            return Poll::Ready(Some(self.deliver_stop(stop)));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                self.received += 1;
                if is_terminal(&event) {
                    self.finished = true;
                }
                Poll::Ready(Some(event))
            }
            Poll::Ready(None) => Poll::Ready(self.on_channel_closed()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Relay {
    /// The channel is closed and drained: the stop terminal when a bound set
    /// one after the caller's last stop check, `None` after a delivered
    /// terminal, else the `internal` failure of a relay task that died.
    fn on_channel_closed(&mut self) -> Option<StreamingEvent> {
        if let Some(stop) = self.stop.take() {
            return Some(self.deliver_stop(stop));
        }
        self.ended = true;
        if self.finished {
            return None;
        }
        self.finished = true;
        self.cancel.cancel();
        tracing::error!(
            response_id = %self.fallback.response_id,
            "stream aborted: the relay task ended without a terminal event"
        );
        let usage = self.fallback.basis.no_final();
        let mut event = self.fallback.failed(
            ResponseError::new("internal", "the response stream terminated unexpectedly"),
            usage,
        );
        set_sequence(&mut event, self.received);
        Some(event)
    }

    /// The `response.failed` of a bound that ended the stream; ends the stream.
    fn deliver_stop(&mut self, Stop { reason, usage }: Stop) -> StreamingEvent {
        self.ended = true;
        self.finished = true;
        let mut event = self.fallback.failed(reason.error(), usage);
        set_sequence(&mut event, self.received);
        event
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
    /// Source of the usage of terminals without final provider usage.
    basis: UsageBasis,
    next_sequence: u64,
}

impl State {
    fn new(params: &StreamParams, basis: UsageBasis) -> Self {
        Self {
            response_id: params.response_id.clone(),
            snapshot: params.snapshot.clone(),
            ignored_parameters: params.ignored_parameters.clone(),
            output_schema: params.output_schema.clone(),
            text: None,
            basis,
            next_sequence: 0,
        }
    }

    /// Number `event` and send it, giving up when a hard bound (shutdown cut,
    /// maximum duration) is reached while the channel is full. The sequence
    /// number is only taken once the slot is reserved, so a given-up event
    /// leaves no gap.
    async fn emit_bounded(
        &mut self,
        tx: &mpsc::Sender<StreamingEvent>,
        event: StreamingEvent,
        bounds: &Bounds,
    ) -> Emit {
        let permit = tokio::select! {
            biased;
            stop = bounds.hard_stop() => return Emit::Stop(stop),
            permit = tx.reserve() => permit,
        };
        match permit {
            Ok(permit) => {
                self.send(permit, event);
                Emit::Sent
            }
            Err(_) => Emit::Gone,
        }
    }

    fn send(&mut self, permit: mpsc::Permit<'_, StreamingEvent>, mut event: StreamingEvent) {
        set_sequence(&mut event, self.next_sequence);
        self.next_sequence += 1;
        permit.send(event);
    }

    fn no_final_usage(&self) -> Usage {
        self.basis.no_final()
    }

    /// Gateway-owned fields of a terminal snapshot: id and merged
    /// `ignored_parameters`.
    fn finish_snapshot(&self, response: &mut ResponseResource, missing: MissingUsage) {
        self.response_id.clone_into(&mut response.id);
        let mut ignored = std::mem::take(&mut response.ignored_parameters);
        ignored.extend(self.ignored_parameters.iter().cloned());
        ignored.sort();
        ignored.dedup();
        response.ignored_parameters = ignored;
        // Every terminal carries one Usage; provider-supplied usage is kept.
        if response.usage.is_none() {
            response.usage = Some(match missing {
                MissingUsage::NoFinal => self.basis.no_final(),
                MissingUsage::ProviderError => self.basis.provider_error(),
            });
        }
    }

    /// A terminal built by the gateway on top of the initial snapshot.
    fn own_terminal(
        &self,
        status: ResponseStatus,
        error: Option<ResponseError>,
        usage: Usage,
    ) -> ResponseResource {
        let mut response = self.snapshot.clone();
        response.status = status;
        response.error = error;
        response.completed_at = Some(now_secs());
        response.usage = Some(usage);
        self.finish_snapshot(&mut response, MissingUsage::NoFinal);
        response
    }

    fn failed(&self, error: ResponseError, usage: Usage) -> StreamingEvent {
        snapshot_event(
            StreamingEvent::Failed,
            self.own_terminal(ResponseStatus::Failed, Some(error), usage),
        )
    }

    /// Map one provider event to the consumer event; `None` drops it
    /// (provider lifecycle snapshots are replaced by the gateway's own).
    fn translate(&mut self, event: StreamingEvent) -> Option<StreamingEvent> {
        Some(match event {
            StreamingEvent::Created(e)
            | StreamingEvent::InProgress(e)
            | StreamingEvent::Queued(e) => {
                if let Some(usage) = e.response.usage {
                    self.basis.note_usage(usage);
                }
                return None;
            }
            StreamingEvent::Error(e) => {
                tracing::warn!(
                    response_id = %self.response_id,
                    code = %e.error.code,
                    "stream aborted: the provider sent an error event"
                );
                let usage = self.basis.provider_error();
                self.failed(e.error, usage)
            }
            StreamingEvent::Completed(e) => self.completed(e.response),
            StreamingEvent::Incomplete(e) => {
                let mut response = e.response;
                self.finish_snapshot(&mut response, MissingUsage::NoFinal);
                snapshot_event(StreamingEvent::Incomplete, response)
            }
            StreamingEvent::Failed(e) => {
                let mut response = e.response;
                self.finish_snapshot(&mut response, MissingUsage::ProviderError);
                snapshot_event(StreamingEvent::Failed, response)
            }
            StreamingEvent::OutputTextDelta(e) => {
                self.basis.note_output(e.text.len());
                if self.output_schema.is_some() {
                    self.text.get_or_insert_with(String::new).push_str(&e.text);
                }
                StreamingEvent::OutputTextDelta(e)
            }
            StreamingEvent::RefusalDelta(e) => {
                self.basis.note_output(e.delta.len());
                StreamingEvent::RefusalDelta(e)
            }
            StreamingEvent::ReasoningDelta(e) => {
                self.basis.note_output(e.delta.len());
                StreamingEvent::ReasoningDelta(e)
            }
            StreamingEvent::ReasoningSummaryTextDelta(e) => {
                self.basis.note_output(e.text.len());
                StreamingEvent::ReasoningSummaryTextDelta(e)
            }
            StreamingEvent::FunctionCallArgumentsDelta(e) => {
                self.basis.note_output(e.arguments.len());
                StreamingEvent::FunctionCallArgumentsDelta(e)
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
                // Keep the provider's usage: the tokens were spent.
                let usage = response
                    .usage
                    .take()
                    .unwrap_or_else(|| self.no_final_usage());
                let mut failed =
                    self.own_terminal(ResponseStatus::Failed, Some(err.to_response_error()), usage);
                failed.provider_details = response.provider_details.take();
                return snapshot_event(StreamingEvent::Failed, failed);
            }
        }
        self.finish_snapshot(&mut response, MissingUsage::NoFinal);
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

/// Provider request id carried by a terminal snapshot's provider details, or
/// by the provider error of a failed terminal.
fn terminal_provider_request_id(event: &StreamingEvent) -> Option<String> {
    match event {
        StreamingEvent::Completed(e)
        | StreamingEvent::Incomplete(e)
        | StreamingEvent::Failed(e) => e
            .response
            .provider_details
            .as_ref()
            .and_then(|details| details.provider_request_id.clone())
            .or_else(|| {
                e.response
                    .error
                    .as_ref()
                    .and_then(|error| error.provider_request_id.clone())
            }),
        _ => None,
    }
}

/// Usage carried by a terminal event (every terminal carries one).
fn terminal_usage(event: &StreamingEvent) -> Option<Usage> {
    match event {
        StreamingEvent::Completed(e)
        | StreamingEvent::Incomplete(e)
        | StreamingEvent::Failed(e) => e.response.usage.clone(),
        _ => None,
    }
}

/// Why a supervised stream was ended by the gateway.
#[derive(Clone, Copy)]
enum StopReason {
    /// No provider event within `timeouts.stream_idle`.
    Idle,
    /// `timeouts.stream_max_duration` passed.
    MaxDuration,
    /// The shutdown drain deadline passed.
    Shutdown,
}

impl StopReason {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::MaxDuration => "max_duration",
            Self::Shutdown => "shutdown",
        }
    }

    /// The `response.failed` error: fixed messages, no provider content.
    fn error(self) -> ResponseError {
        match self {
            Self::Idle => ResponseError::new(
                "provider_unavailable",
                "the provider sent no event within the idle timeout",
            ),
            Self::MaxDuration => ResponseError::new(
                "provider_unavailable",
                "the response exceeded the maximum stream duration",
            ),
            Self::Shutdown => {
                ResponseError::new("gateway_unavailable", "the gateway is shutting down")
            }
        }
    }
}

/// A bound ended the stream: the reason and the usage the lease was settled
/// with.
struct Stop {
    reason: StopReason,
    usage: Usage,
}

/// Hand-over of a [`Stop`] from the relay task to the consumer side.
#[derive(Clone, Default)]
struct StopCell(Arc<Mutex<Option<Stop>>>);

impl StopCell {
    fn set(&self, stop: Stop) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stop);
    }

    fn take(&self) -> Option<Stop> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Outcome of a bounded send.
enum Emit {
    Sent,
    /// The consumer is gone.
    Gone,
    Stop(StopReason),
}

/// The bounds of one relay, resolved at its start.
struct Bounds {
    idle: Option<Duration>,
    /// `max_duration` from the relay start.
    deadline: Option<tokio::time::Instant>,
    cut: Option<CancellationToken>,
}

impl Bounds {
    fn new(limits: StreamLimits) -> Self {
        Self {
            idle: limits.idle_timeout,
            deadline: limits
                .max_duration
                .map(|max| tokio::time::Instant::now() + max),
            cut: limits.shutdown_cut,
        }
    }

    /// Resolves when the shutdown cut or the maximum duration is reached;
    /// never when neither is set. Cancel-safe.
    async fn hard_stop(&self) -> StopReason {
        tokio::select! {
            biased;
            () = async {
                match &self.cut {
                    Some(cut) => cut.cancelled().await,
                    None => std::future::pending().await,
                }
            } => StopReason::Shutdown,
            () = async {
                match self.deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => StopReason::MaxDuration,
        }
    }

    /// Resolves after the idle timeout; created anew for every wait on the
    /// plugin stream, so only that wait counts.
    async fn idle_expired(&self) -> StopReason {
        match self.idle {
            Some(idle) => tokio::time::sleep(idle).await,
            None => std::future::pending().await,
        }
        StopReason::Idle
    }
}

/// Drive `upstream` into `tx` until a terminal event was sent or the consumer
/// left. The terminal hook runs once: with the sent terminal's usage, or, on
/// every other exit (disconnect, task death), with the estimate.
async fn relay(
    mut upstream: ResponseEventStream,
    params: StreamParams,
    tx: mpsc::Sender<StreamingEvent>,
    basis: UsageBasis,
    stop: StopCell,
) {
    let mut state = State::new(&params, basis.clone());
    // Released when the task ends, whichever way.
    let _slot = params.slot;
    let mut guard = TerminalGuard {
        basis,
        hook: params.on_terminal,
    };
    let bounds = Bounds::new(params.limits);
    let ctx = StopCtx {
        bounds: &bounds,
        cancel: &params.cancel,
        stop: &stop,
    };
    let created = snapshot_event(StreamingEvent::Created, state.snapshot.clone());
    let in_progress = snapshot_event(StreamingEvent::InProgress, state.snapshot.clone());
    for event in [created, in_progress] {
        match state.emit_bounded(&tx, event, &bounds).await {
            Emit::Sent => {}
            Emit::Gone => {
                log_disconnect(&state.response_id);
                return;
            }
            Emit::Stop(reason) => {
                stop_stream(&state, &mut guard, &params.cancel, &stop, reason, None);
                return;
            }
        }
    }

    loop {
        // Only the wait on the plugin stream counts for the idle timer.
        let next = tokio::select! {
            biased;
            () = tx.closed() => {
                log_disconnect(&state.response_id);
                return;
            }
            reason = bounds.hard_stop() => {
                stop_stream(&state, &mut guard, &params.cancel, &stop, reason, None);
                return;
            }
            reason = bounds.idle_expired() => {
                stop_stream(&state, &mut guard, &params.cancel, &stop, reason, None);
                return;
            }
            next = upstream.next() => next,
        };
        let Some(event) = next else {
            tracing::warn!(
                response_id = %state.response_id,
                "stream aborted: the provider stream ended without a terminal event"
            );
            let usage = state.no_final_usage();
            let response = state.own_terminal(ResponseStatus::Completed, None, usage);
            let terminal = state.completed(response);
            send_terminal(&mut state, &tx, &mut guard, &ctx, terminal).await;
            return;
        };
        let Some(outgoing) = state.translate(event) else {
            continue;
        };
        if is_terminal(&outgoing) {
            send_terminal(&mut state, &tx, &mut guard, &ctx, outgoing).await;
            return;
        }
        match state.emit_bounded(&tx, outgoing, &bounds).await {
            Emit::Sent => {}
            Emit::Gone => {
                log_disconnect(&state.response_id);
                return;
            }
            Emit::Stop(reason) => {
                stop_stream(&state, &mut guard, &params.cancel, &stop, reason, None);
                return;
            }
        }
    }
}

/// End the stream because a bound was reached: cancel the request, settle
/// (with `settled`, else the estimate) and hand the `response.failed` to the
/// consumer side (see [`Relay`]), which delivers it without waiting for the
/// channel to drain. The plugin stream is dropped when the relay returns.
fn stop_stream(
    state: &State,
    guard: &mut TerminalGuard,
    cancel: &CancellationToken,
    stop: &StopCell,
    reason: StopReason,
    settled: Option<(Usage, Option<String>)>,
) {
    cancel.cancel();
    tracing::warn!(
        response_id = %state.response_id,
        reason = reason.label(),
        "stream aborted: a stream bound was reached"
    );
    let (usage, provider_request_id) = settled.unwrap_or_else(|| (state.no_final_usage(), None));
    guard.finish(usage.clone(), provider_request_id);
    stop.set(Stop { reason, usage });
}

/// What a terminal send needs to end the stream at a bound.
struct StopCtx<'a> {
    bounds: &'a Bounds,
    cancel: &'a CancellationToken,
    stop: &'a StopCell,
}

/// Send the terminal event, bounded by the shutdown cut and the maximum
/// duration; once it is in the channel the stream is settled with its usage.
/// A bound reached while the channel is full ends the stream like
/// [`stop_stream`], settled with the terminal's own usage and provider request
/// id (the estimate when it has none). A consumer that left first is a
/// disconnect (the guard's estimate applies).
async fn send_terminal(
    state: &mut State,
    tx: &mpsc::Sender<StreamingEvent>,
    guard: &mut TerminalGuard,
    ctx: &StopCtx<'_>,
    terminal: StreamingEvent,
) {
    let usage = terminal_usage(&terminal);
    let provider_request_id = terminal_provider_request_id(&terminal);
    match state.emit_bounded(tx, terminal, ctx.bounds).await {
        Emit::Sent => {
            if let Some(usage) = usage {
                guard.finish(usage, provider_request_id);
            }
        }
        Emit::Gone => log_disconnect(&state.response_id),
        Emit::Stop(reason) => {
            let settled = usage.map(|usage| (usage, provider_request_id));
            stop_stream(state, guard, ctx.cancel, ctx.stop, reason, settled);
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "stream_terminal_tests.rs"]
mod stream_terminal_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "stream_timeout_tests.rs"]
mod stream_timeout_tests;
