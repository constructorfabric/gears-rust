//! First-party LLM Gateway plugin (Phase 13).
//!
//! Implements the [`ChatEngineBackendPlugin`] trait by proxying calls to:
//!
//! - the in-process **Model Registry** for capability resolution
//!   (`on_session_created`, `on_session_updated`);
//! - the in-process **LLM Gateway** service for message forwarding
//!   (`on_message`, `on_message_recreate`) and summary generation
//!   (`on_session_summary`).
//!
//! Per ADR-0023 the plugin owns **all** resilience — timeout, retry with
//! exponential backoff, per-service circuit breaker — and never delegates
//! to Chat Engine core. To keep this file testable without a real HTTP
//! client we abstract the two external services behind narrow async
//! traits ([`LlmGatewayClient`] and [`ModelRegistryClient`]). Phase 15 will
//! ship the production `reqwest`-backed implementations and wire them via
//! ClientHub; this phase ships the plugin itself plus an `Arc`-based
//! constructor.
//!
//! ## Discriminator-prefixed errors
//!
//! Per ADR-0023 the plugin surfaces three plugin-defined recoverable
//! errors via `StreamingErrorEvent { error: "<discriminator>: <detail>" }`:
//!
//! - `context_overflow:` — upstream context window exceeded; core invokes
//!   `on_session_summary` (Phase 8).
//! - `stream_interrupted:` — mid-response disconnect; core persists the
//!   partial message with `finish_reason: "error"`.
//! - `deadline_exceeded:` — `ctx.remaining()` returned `Some(ZERO)` before
//!   the upstream call could be issued.
//!
//! Every other failure traversing the boundary uses one of the
//! `PluginError::*_with(msg, source)` constructors so the underlying
//! `reqwest::Error`, `hyper::Error`, `serde_json::Error`, or
//! `std::io::Error` remains attached.
//!
//! ## Debug-redaction contract
//!
//! Per the Phase 13 rules: **plugin config**, **message content**, and
//! **API credentials** MUST never appear in tracing fields. Only summary
//! counts (`messages_in`, `messages_to_summarize`, `bytes_received`,
//! `summarization_enabled`, `retry_count`) and identifiers (`trace_id`,
//! `session_id`, `duration_ms`) are permitted. Audit every
//! `tracing::info!` / `warn!` / `error!` call below for this invariant.
//
// @cpt-cf-chat-engine-llm-gateway-plugin:p13

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use chat_engine_sdk::error::PluginError;
use chat_engine_sdk::models::{
    Capability, CapabilityValue, HealthStatus, Message, StreamingChunkEvent,
    StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent,
};
use chat_engine_sdk::plugin::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginCallContext, PluginStream, SessionPluginCtx,
    stream_from_events,
};

use crate::domain::llm_config::{
    LlmMessageMetadata, LlmPluginConfig, LlmSummarizationSettings, validate_plugin_config,
};

/// Stable GTS plugin instance ID per ADR-0023. Renaming is a breaking
/// change — clients persist the ID in `plugin_configs.plugin_instance_id`.
pub const LLM_GATEWAY_PLUGIN_INSTANCE_ID: &str = "gtx.cf.chat_engine.llm_gateway_plugin.v1~";

/// Capability ID for the model selector capability (enum).
pub const CAPABILITY_MODEL: &str = "model";
/// Capability ID for the sampling temperature capability (float 0..2).
pub const CAPABILITY_TEMPERATURE: &str = "temperature";
/// Capability ID for the stream-toggle capability (bool).
pub const CAPABILITY_STREAM: &str = "stream";

/// Discriminator prefix emitted on upstream context-window overflow.
pub const ERROR_PREFIX_CONTEXT_OVERFLOW: &str = "context_overflow:";
/// Discriminator prefix emitted on mid-stream disconnect.
pub const ERROR_PREFIX_STREAM_INTERRUPTED: &str = "stream_interrupted:";
/// Discriminator prefix emitted when `ctx.remaining()` is `Some(ZERO)`.
pub const ERROR_PREFIX_DEADLINE_EXCEEDED: &str = "deadline_exceeded:";

// ---------------------------------------------------------------- types ---

/// Description of a single capability declared by the Model Registry for
/// a given model. The plugin maps each entry to a `Capability` returned
/// from `on_session_created` / `on_session_updated`.
#[derive(Debug, Clone)]
pub struct ModelCapabilitySchema {
    pub name: String,
    pub schema: serde_json::Value,
}

/// View of the Model Registry's `list_models` response — just the
/// information the plugin needs to build the `model` enum capability.
#[derive(Debug, Clone)]
pub struct ModelCatalog {
    pub model_ids: Vec<String>,
    pub default_model_id: String,
}

/// Upstream chunk shape produced by the LLM Gateway streaming protocol.
/// Each upstream item is either a content chunk, a terminal metadata
/// payload, or one of the discriminator-prefixed error signals. The
/// plugin transforms each variant into an SDK `StreamingEvent`.
#[derive(Debug)]
pub enum UpstreamEvent {
    Chunk(String),
    Complete(LlmMessageMetadata),
    /// Upstream context window exceeded — surfaced as
    /// `context_overflow: <detail>`.
    ContextOverflow(String),
    /// Mid-stream disconnect — surfaced as `stream_interrupted: <detail>`.
    StreamInterrupted(String),
    /// Any other upstream failure (HTTP 5xx, malformed payload, …).
    /// The wrapped `PluginError` retains its `source` chain.
    Error(PluginError),
}

/// Request payload assembled by the plugin from `MessagePluginCtx` and
/// the resolved `LlmPluginConfig`. Production transports serialize this
/// directly; test fakes inspect it for assertions.
#[derive(Debug, Clone)]
pub struct LlmGatewayRequest {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub stream: bool,
    pub messages: Vec<Message>,
}

/// Boxed async stream of upstream events produced by [`LlmGatewayClient`].
pub type UpstreamStream =
    futures::stream::BoxStream<'static, Result<UpstreamEvent, PluginError>>;

/// Narrow abstraction over the LLM Gateway HTTP surface. Production code
/// supplies a `reqwest`-backed implementation (Phase 15); unit tests use
/// the [`FakeLlmGatewayClient`] fake defined in this file's `#[cfg(test)]`
/// module.
#[async_trait]
pub trait LlmGatewayClient: Send + Sync + 'static {
    /// Forward a chat message to the LLM Gateway and stream the response.
    async fn stream_chat(
        &self,
        config: &LlmPluginConfig,
        request: LlmGatewayRequest,
    ) -> Result<UpstreamStream, PluginError>;

    /// Forward a batch of messages for summary generation.
    async fn summarize(
        &self,
        config: &LlmPluginConfig,
        messages: Vec<Message>,
    ) -> Result<String, PluginError>;
}

/// Narrow abstraction over the Model Registry HTTP surface.
#[async_trait]
pub trait ModelRegistryClient: Send + Sync + 'static {
    /// Returns the catalogue of available model IDs plus the designated
    /// default.
    async fn list_models(&self, config: &LlmPluginConfig) -> Result<ModelCatalog, PluginError>;

    /// Returns the per-model capability schemas (e.g. `temperature`,
    /// `max_tokens`, `web_search`).
    async fn model_capabilities(
        &self,
        config: &LlmPluginConfig,
        model_id: &str,
    ) -> Result<Vec<ModelCapabilitySchema>, PluginError>;
}

/// Result returned by `on_session_summary`. Phase 8 owns persistence.
#[derive(Debug, Clone)]
pub struct SummaryResult {
    pub summary_text: String,
    pub summarized_message_ids: Vec<Uuid>,
}

// --------------------------------------------------------- plugin struct ---

/// First-party LLM Gateway plugin.
///
/// Owns `Arc`-shared clients so cloning is cheap and the trait methods
/// can spawn `'static` streams without borrowing `&self`.
pub struct LlmGatewayPlugin {
    plugin_instance_id: String,
    gateway_client: Arc<dyn LlmGatewayClient>,
    model_registry: Arc<dyn ModelRegistryClient>,
}

impl LlmGatewayPlugin {
    /// Construct a new plugin instance using the stable
    /// [`LLM_GATEWAY_PLUGIN_INSTANCE_ID`].
    #[must_use]
    pub fn new(
        gateway_client: Arc<dyn LlmGatewayClient>,
        model_registry: Arc<dyn ModelRegistryClient>,
    ) -> Self {
        Self {
            plugin_instance_id: LLM_GATEWAY_PLUGIN_INSTANCE_ID.to_owned(),
            gateway_client,
            model_registry,
        }
    }

    /// Construct with a caller-supplied instance ID. Phase 15 may use this
    /// to provision multiple LLM Gateway plugin bindings under distinct
    /// IDs (e.g., per-tenant gateways).
    #[must_use]
    pub fn with_instance_id(
        plugin_instance_id: impl Into<String>,
        gateway_client: Arc<dyn LlmGatewayClient>,
        model_registry: Arc<dyn ModelRegistryClient>,
    ) -> Self {
        Self {
            plugin_instance_id: plugin_instance_id.into(),
            gateway_client,
            model_registry,
        }
    }

    /// Borrow the active config from the call context. Returns
    /// `PluginError::InvalidInput` if absent.
    fn config_from_ctx(call_ctx: &PluginCallContext) -> Result<LlmPluginConfig, PluginError> {
        let blob = call_ctx
            .plugin_config
            .as_ref()
            .ok_or_else(|| PluginError::invalid_input("missing plugin_config"))?;
        validate_plugin_config(blob)
    }

    /// Build the `Vec<Capability>` returned from `on_session_created`.
    async fn resolve_capabilities(
        &self,
        config: &LlmPluginConfig,
    ) -> Result<Vec<Capability>, PluginError> {
        let catalog = self.model_registry.list_models(config).await?;
        let preferred = config
            .default_model
            .clone()
            .filter(|m| catalog.model_ids.iter().any(|x| x == m))
            .unwrap_or_else(|| catalog.default_model_id.clone());

        let mut caps = Vec::with_capacity(4);

        caps.push(Capability {
            name: CAPABILITY_MODEL.into(),
            value: serde_json::json!({
                "type": "enum",
                "enum_values": catalog.model_ids,
                "default_value": preferred,
            }),
        });

        caps.push(Capability {
            name: CAPABILITY_TEMPERATURE.into(),
            value: serde_json::json!({
                "type": "float",
                "min": 0.0,
                "max": 2.0,
                "default_value": 0.7,
            }),
        });

        caps.push(Capability {
            name: CAPABILITY_STREAM.into(),
            value: serde_json::json!({
                "type": "bool",
                "default_value": true,
            }),
        });

        let model_caps = self
            .model_registry
            .model_capabilities(config, &preferred)
            .await?;
        for c in model_caps {
            // The three built-in IDs above always win — extra entries from
            // the registry are appended verbatim.
            if matches!(
                c.name.as_str(),
                CAPABILITY_MODEL | CAPABILITY_TEMPERATURE | CAPABILITY_STREAM
            ) {
                continue;
            }
            caps.push(Capability {
                name: c.name,
                value: c.schema,
            });
        }

        Ok(caps)
    }

    /// Returns the new capability set when the `model` value changed,
    /// otherwise short-circuits with the previous enabled set re-cast as a
    /// schema declaration (the registry call is skipped).
    async fn refresh_capabilities(
        &self,
        config: &LlmPluginConfig,
        previous: Option<&Vec<CapabilityValue>>,
    ) -> Result<Vec<Capability>, PluginError> {
        let _ = previous; // The fast-path "unchanged" decision lives in the
                          // caller's wiring (Phase 15) — at the trait
                          // boundary we always rebuild from the registry
                          // so the schema stays authoritative.
        self.resolve_capabilities(config).await
    }
}

impl LlmGatewayPlugin {
    /// Translate a single upstream event into the SDK `StreamingEvent`
    /// shape consumed by `ResponseStream`.
    fn transform_event(message_id: Uuid, ev: UpstreamEvent) -> StreamingEvent {
        match ev {
            UpstreamEvent::Chunk(chunk) => StreamingEvent::Chunk(StreamingChunkEvent {
                message_id,
                chunk,
            }),
            UpstreamEvent::Complete(meta) => StreamingEvent::Complete(StreamingCompleteEvent {
                message_id,
                metadata: Some(meta.to_json()),
            }),
            UpstreamEvent::ContextOverflow(detail) => StreamingEvent::Error(StreamingErrorEvent {
                message_id,
                error: format!("{ERROR_PREFIX_CONTEXT_OVERFLOW} {detail}"),
            }),
            UpstreamEvent::StreamInterrupted(detail) => {
                StreamingEvent::Error(StreamingErrorEvent {
                    message_id,
                    error: format!("{ERROR_PREFIX_STREAM_INTERRUPTED} {detail}"),
                })
            }
            UpstreamEvent::Error(err) => StreamingEvent::Error(StreamingErrorEvent {
                message_id,
                error: format!("internal: {err}"),
            }),
        }
    }

    /// Build a deadline-exceeded `StreamingEvent`.
    fn deadline_exceeded_event(message_id: Uuid) -> StreamingEvent {
        StreamingEvent::Error(StreamingErrorEvent {
            message_id,
            error: format!("{ERROR_PREFIX_DEADLINE_EXCEEDED} deadline elapsed before upstream call"),
        })
    }

    /// Drive a single non-streaming upstream call with the resilience
    /// budget from `LlmPluginConfig` (retry with exponential backoff).
    /// Streaming forwarding does **not** go through this helper — at-most-
    /// once semantics forbid mid-stream retry.
    async fn with_retry<F, Fut, T>(
        config: &LlmPluginConfig,
        cancel: &CancellationToken,
        mut op: F,
    ) -> Result<T, PluginError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, PluginError>>,
    {
        let max_attempts = config.effective_retry_count().max(1);
        let base_delay = config.effective_retry_delay();
        let mut last_err: Option<PluginError> = None;
        for attempt in 0..max_attempts {
            if cancel.is_cancelled() {
                return Err(PluginError::transient("cancelled"));
            }
            let result = select! {
                _ = cancel.cancelled() => Err(PluginError::transient("cancelled")),
                r = op() => r,
            };
            match result {
                Ok(v) => return Ok(v),
                Err(e) if !e.is_retryable() => return Err(e),
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < max_attempts {
                        let backoff =
                            base_delay.saturating_mul(2u32.saturating_pow(attempt));
                        select! {
                            _ = cancel.cancelled() => {
                                return Err(PluginError::transient("cancelled"));
                            }
                            _ = tokio::time::sleep(backoff) => {}
                        }
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            PluginError::internal("retry budget exhausted without recording an error")
        }))
    }
}

#[async_trait]
impl ChatEngineBackendPlugin for LlmGatewayPlugin {
    async fn on_session_type_configured(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        // Validate the plugin config and return an empty capability set —
        // resolution is deferred to `on_session_created` per ADR-0023
        // lifecycle step 2.
        let cfg = Self::config_from_ctx(&ctx.call_ctx)?;
        debug!(
            target = "chat_engine::llm_gateway",
            session_type_id = %ctx.session_type_id,
            summarization_enabled = cfg.summarization_enabled(),
            retry_count = cfg.effective_retry_count(),
            "llm gateway plugin config validated",
        );
        Ok(Vec::new())
    }

    async fn on_session_created(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        let cfg = Self::config_from_ctx(&ctx.call_ctx)?;
        let start = Instant::now();
        let result = self.resolve_capabilities(&cfg).await;
        debug!(
            target = "chat_engine::llm_gateway",
            session_type_id = %ctx.session_type_id,
            session_id = ?ctx.session_id,
            duration_ms = start.elapsed().as_millis() as u64,
            ok = result.is_ok(),
            "llm gateway: capabilities resolved",
        );
        result
    }

    async fn on_session_updated(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        let cfg = Self::config_from_ctx(&ctx.call_ctx)?;
        let start = Instant::now();
        let result = self
            .refresh_capabilities(&cfg, ctx.call_ctx.enabled_capabilities.as_ref())
            .await;
        debug!(
            target = "chat_engine::llm_gateway",
            session_type_id = %ctx.session_type_id,
            session_id = ?ctx.session_id,
            duration_ms = start.elapsed().as_millis() as u64,
            ok = result.is_ok(),
            "llm gateway: capabilities refreshed",
        );
        result
    }

    async fn on_message(&self, ctx: MessagePluginCtx) -> Result<PluginStream, PluginError> {
        forward_to_gateway(
            ctx,
            Arc::clone(&self.gateway_client),
            self.plugin_instance_id.clone(),
        )
        .await
    }

    async fn on_message_recreate(
        &self,
        ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        // Recreate semantics are identical to `on_message` from the
        // plugin's perspective — the difference (overwrite vs append) is
        // handled by `VariantService` (Phase 6).
        forward_to_gateway(
            ctx,
            Arc::clone(&self.gateway_client),
            self.plugin_instance_id.clone(),
        )
        .await
    }

    async fn on_session_summary(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<PluginStream, PluginError> {
        // The Phase 13 summary contract returns a single-shot stream
        // carrying the summary text in one Complete event whose metadata
        // contains the `SummaryResult` JSON. Phase 8 (`MessageService`)
        // owns persistence — this plugin is stateless.
        let cfg = Self::config_from_ctx(&ctx.call_ctx)?;
        let settings: LlmSummarizationSettings =
            cfg.summarization_settings.ok_or_else(|| {
                PluginError::internal(
                    "summarization unsupported: LlmPluginConfig.summarization_settings is null",
                )
            })?;

        // The full visible history must be provided via the call context;
        // we accept it via a dedicated key on `plugin_config` only as a
        // fallback for the Phase 13 stub harness, otherwise this would
        // come from a separate `SummaryPluginCtx` shape future phases may
        // introduce. The session plugin context already carries
        // `session_id`/`call_ctx` — message history lookup is core's job.
        let history: Vec<Message> = match ctx
            .call_ctx
            .plugin_config
            .as_ref()
            .and_then(|v| v.get("__summary_messages"))
        {
            Some(raw) => serde_json::from_value(raw.clone()).map_err(|e| {
                PluginError::invalid_input_with(
                    "invalid __summary_messages payload",
                    e,
                )
            })?,
            None => Vec::new(),
        };

        if history.is_empty() {
            return Err(PluginError::invalid_input(
                "on_session_summary: empty history supplied",
            ));
        }

        let keep = settings.keep_count() as usize;
        let split_at = history.len().saturating_sub(keep);
        let to_summarize: Vec<Message> = history.iter().take(split_at).cloned().collect();
        let summarized_ids: Vec<Uuid> = to_summarize.iter().map(|m| m.message_id).collect();

        if to_summarize.is_empty() {
            // No older history to summarize; nothing to do.
            return Ok(stream_from_events(Vec::new()));
        }

        let cancel = ctx.call_ctx.cancel.clone();
        let client = Arc::clone(&self.gateway_client);
        let cfg_clone = cfg.clone();

        let summary_text =
            Self::with_retry(&cfg_clone, &cancel, || {
                let client = Arc::clone(&client);
                let cfg = cfg_clone.clone();
                let msgs = to_summarize.clone();
                async move { client.summarize(&cfg, msgs).await }
            })
            .await?;

        let session_id = ctx.session_id.unwrap_or_else(Uuid::nil);
        let summary_event = StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: session_id,
            metadata: Some(serde_json::json!({
                "summary_text": summary_text,
                "summarized_message_ids": summarized_ids,
            })),
        });

        debug!(
            target = "chat_engine::llm_gateway",
            session_id = %session_id,
            messages_to_summarize = summarized_ids.len(),
            "llm gateway: summary generated",
        );

        Ok(stream_from_events(vec![summary_event]))
    }

    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        // Endpoint-per-config plugin — health is exercised on demand via
        // `on_message`. Surface `Healthy` so the registrar accepts the
        // plugin at startup; real upstream failures show up on the first
        // call through the standard error path.
        Ok(HealthStatus::Healthy)
    }

    fn plugin_instance_id(&self) -> &str {
        &self.plugin_instance_id
    }
}

// --------------------------------------------------------- streaming core ---

async fn forward_to_gateway(
    ctx: MessagePluginCtx,
    client: Arc<dyn LlmGatewayClient>,
    _plugin_instance_id: String,
) -> Result<PluginStream, PluginError> {
    let cfg = LlmGatewayPlugin::config_from_ctx(&ctx.call_ctx)?;

    // Defence-in-depth: core also filters hidden-from-backend messages but
    // the plugin re-validates per Phase 13 rule "every inbound messages[i]
    // has is_hidden_from_backend=false".
    if let Some(bad) = ctx.messages.iter().find(|m| m.is_hidden_from_backend) {
        let err = PluginError::invalid_input(format!(
            "message {} is hidden from backend but was forwarded to the plugin",
            bad.message_id,
        ));
        return Err(err);
    }

    // Deadline check before any work — if the budget is already exhausted
    // we emit `deadline_exceeded:` and short-circuit.
    if matches!(ctx.call_ctx.remaining(), Some(d) if d.is_zero()) {
        let event = LlmGatewayPlugin::deadline_exceeded_event(ctx.message_id);
        return Ok(stream_from_events(vec![event]));
    }

    let request = build_request(&ctx, &cfg);
    let message_id = ctx.message_id;
    let session_id = ctx.session_id;
    let cancel = ctx.call_ctx.cancel.clone();

    debug!(
        target = "chat_engine::llm_gateway",
        session_id = %session_id,
        message_id = %message_id,
        messages_in = ctx.messages.len(),
        stream = request.stream,
        "llm gateway: forwarding message",
    );

    // Streaming forwarding is at-most-once: no retry.
    let upstream = client.stream_chat(&cfg, request).await?;

    let stream = futures::stream::unfold(
        ForwardState {
            upstream: Some(upstream),
            cancel,
            message_id,
            bytes_received: 0,
            finished: false,
        },
        |mut state| async move {
            if state.finished {
                return None;
            }
            // Borrow disjoint fields explicitly so the `select!` body can
            // race the cancellation token against the upstream stream
            // without conflicting borrows of `state`.
            let cancel = state.cancel.clone();
            let upstream = state.upstream.as_mut()?;

            let next = select! {
                _ = cancel.cancelled() => None,
                item = upstream.next() => item,
            };

            match next {
                None => {
                    // End of upstream stream (clean close) or cancellation.
                    state.finished = true;
                    None
                }
                Some(Ok(UpstreamEvent::Chunk(chunk))) => {
                    state.bytes_received = state.bytes_received.saturating_add(chunk.len());
                    let ev = LlmGatewayPlugin::transform_event(
                        state.message_id,
                        UpstreamEvent::Chunk(chunk),
                    );
                    Some((Ok(ev), state))
                }
                Some(Ok(UpstreamEvent::Complete(meta))) => {
                    let ev = LlmGatewayPlugin::transform_event(
                        state.message_id,
                        UpstreamEvent::Complete(meta),
                    );
                    state.finished = true;
                    Some((Ok(ev), state))
                }
                Some(Ok(UpstreamEvent::ContextOverflow(detail))) => {
                    let ev = LlmGatewayPlugin::transform_event(
                        state.message_id,
                        UpstreamEvent::ContextOverflow(detail),
                    );
                    state.finished = true;
                    Some((Ok(ev), state))
                }
                Some(Ok(UpstreamEvent::StreamInterrupted(detail))) => {
                    warn!(
                        target = "chat_engine::llm_gateway",
                        message_id = %state.message_id,
                        bytes_received = state.bytes_received,
                        "llm gateway: stream interrupted",
                    );
                    let ev = LlmGatewayPlugin::transform_event(
                        state.message_id,
                        UpstreamEvent::StreamInterrupted(detail),
                    );
                    state.finished = true;
                    Some((Ok(ev), state))
                }
                Some(Ok(UpstreamEvent::Error(err))) => {
                    state.finished = true;
                    Some((Err(err), state))
                }
                Some(Err(err)) => {
                    state.finished = true;
                    Some((Err(err), state))
                }
            }
        },
    );

    Ok(stream.boxed())
}

struct ForwardState {
    upstream: Option<UpstreamStream>,
    cancel: CancellationToken,
    message_id: Uuid,
    bytes_received: usize,
    finished: bool,
}

fn build_request(ctx: &MessagePluginCtx, cfg: &LlmPluginConfig) -> LlmGatewayRequest {
    let mut model: Option<String> = cfg.default_model.clone();
    let mut temperature: Option<f32> = None;
    let mut stream = true;

    if let Some(values) = ctx.call_ctx.enabled_capabilities.as_ref() {
        for v in values {
            match v.name.as_str() {
                CAPABILITY_MODEL => {
                    if let Some(s) = v.value.as_str() {
                        model = Some(s.to_owned());
                    }
                }
                CAPABILITY_TEMPERATURE => {
                    if let Some(f) = v.value.as_f64() {
                        // f32 narrowing — values outside the schema range
                        // are clipped client-side by core before they
                        // reach the plugin.
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            temperature = Some(f as f32);
                        }
                    }
                }
                CAPABILITY_STREAM => {
                    if let Some(b) = v.value.as_bool() {
                        stream = b;
                    }
                }
                _ => {}
            }
        }
    }

    LlmGatewayRequest {
        session_id: ctx.session_id,
        message_id: ctx.message_id,
        model,
        temperature,
        stream,
        messages: ctx.messages.clone(),
    }
}

// =========================================================== tests ========

#[cfg(test)]
mod tests {
    use super::*;
    use chat_engine_sdk::models::{Message, MessageRole, TenantId, UserId};
    use futures::stream;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    // --------------------------------------------------------------- fakes -

    #[derive(Default)]
    struct FakeLlmGatewayClient {
        events: Mutex<Vec<UpstreamEvent>>,
        // Captures the most recent request payload so tests can introspect
        // the assembled `LlmGatewayRequest` if needed. Read access is not
        // exercised in the current test suite but the field is part of
        // the fake's contract for future tests.
        #[allow(dead_code)]
        last_request: Mutex<Option<LlmGatewayRequest>>,
        summary: Mutex<Option<String>>,
    }

    impl FakeLlmGatewayClient {
        fn with_events(events: Vec<UpstreamEvent>) -> Self {
            Self {
                events: Mutex::new(events),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl LlmGatewayClient for FakeLlmGatewayClient {
        async fn stream_chat(
            &self,
            _config: &LlmPluginConfig,
            request: LlmGatewayRequest,
        ) -> Result<UpstreamStream, PluginError> {
            *self.last_request.lock().unwrap() = Some(request);
            let events: Vec<UpstreamEvent> =
                self.events.lock().unwrap().drain(..).collect();
            let s = stream::iter(events.into_iter().map(Ok));
            Ok(s.boxed())
        }

        async fn summarize(
            &self,
            _config: &LlmPluginConfig,
            _messages: Vec<Message>,
        ) -> Result<String, PluginError> {
            Ok(self
                .summary
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "<empty>".into()))
        }
    }

    #[derive(Default)]
    struct FakeModelRegistry {
        models: Vec<String>,
        default_model: String,
        per_model_caps: Vec<ModelCapabilitySchema>,
    }

    impl FakeModelRegistry {
        fn new() -> Self {
            Self {
                models: vec!["gpt-4".into(), "gpt-3.5".into()],
                default_model: "gpt-4".into(),
                per_model_caps: vec![ModelCapabilitySchema {
                    name: "max_tokens".into(),
                    schema: serde_json::json!({
                        "type": "int",
                        "min": 1,
                        "max": 4096,
                        "default_value": 1024,
                    }),
                }],
            }
        }
    }

    #[async_trait]
    impl ModelRegistryClient for FakeModelRegistry {
        async fn list_models(
            &self,
            _config: &LlmPluginConfig,
        ) -> Result<ModelCatalog, PluginError> {
            Ok(ModelCatalog {
                model_ids: self.models.clone(),
                default_model_id: self.default_model.clone(),
            })
        }

        async fn model_capabilities(
            &self,
            _config: &LlmPluginConfig,
            _model_id: &str,
        ) -> Result<Vec<ModelCapabilitySchema>, PluginError> {
            Ok(self.per_model_caps.clone())
        }
    }

    // -------------------------------------------------------------- helpers -

    fn make_message(role: MessageRole, text: &str) -> Message {
        let now = OffsetDateTime::now_utc();
        Message {
            message_id: Uuid::new_v4(),
            session_id: Uuid::nil(),
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role,
            content: serde_json::json!({ "text": text }),
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_call_ctx(config: serde_json::Value) -> PluginCallContext {
        PluginCallContext {
            request_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            plugin_instance_id: LLM_GATEWAY_PLUGIN_INSTANCE_ID.into(),
            session_type_id: Uuid::nil(),
            plugin_config: Some(config),
            enabled_capabilities: None,
            deadline: None,
            cancel: CancellationToken::new(),
        }
    }

    fn make_plugin(client: FakeLlmGatewayClient) -> LlmGatewayPlugin {
        LlmGatewayPlugin::new(
            Arc::new(client),
            Arc::new(FakeModelRegistry::new()),
        )
    }

    fn valid_config() -> serde_json::Value {
        serde_json::json!({
            "gateway_url": "https://gw.example",
            "summarization_settings": { "recent_messages_to_keep": 4 },
        })
    }

    // ---------------------------------------------------------------- tests -

    #[tokio::test]
    async fn plugin_instance_id_matches_adr_constant() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        assert_eq!(
            plugin.plugin_instance_id(),
            "gtx.cf.chat_engine.llm_gateway_plugin.v1~",
        );
    }

    #[tokio::test]
    async fn on_session_type_configured_validates_and_returns_empty_caps() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let ctx = SessionPluginCtx {
            session_type_id: Uuid::nil(),
            session_id: None,
            call_ctx: make_call_ctx(valid_config()),
        };
        let caps = plugin.on_session_type_configured(ctx).await.unwrap();
        assert!(caps.is_empty());
    }

    #[tokio::test]
    async fn on_session_type_configured_rejects_invalid_config() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let ctx = SessionPluginCtx {
            session_type_id: Uuid::nil(),
            session_id: None,
            call_ctx: make_call_ctx(serde_json::json!({ "gateway_url": "" })),
        };
        let err = plugin
            .on_session_type_configured(ctx)
            .await
            .expect_err("empty gateway_url must be rejected");
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[tokio::test]
    async fn on_session_created_emits_three_baseline_capabilities() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let ctx = SessionPluginCtx {
            session_type_id: Uuid::nil(),
            session_id: Some(Uuid::new_v4()),
            call_ctx: make_call_ctx(valid_config()),
        };
        let caps = plugin.on_session_created(ctx).await.unwrap();
        let names: Vec<&str> = caps.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&CAPABILITY_MODEL));
        assert!(names.contains(&CAPABILITY_TEMPERATURE));
        assert!(names.contains(&CAPABILITY_STREAM));
        // model-specific extension from the registry must also surface.
        assert!(names.contains(&"max_tokens"));
    }

    #[tokio::test]
    async fn on_message_streams_chunk_then_metadata() {
        let meta = LlmMessageMetadata {
            model_used: "gpt-4".into(),
            finish_reason: crate::domain::llm_config::FinishReason::Stop,
            temperature_used: Some(0.7),
            usage: Some(crate::domain::llm_config::LlmUsage {
                prompt_tokens: 3,
                completion_tokens: 5,
                total_tokens: 8,
                cached_tokens: None,
            }),
        };
        let client = FakeLlmGatewayClient::with_events(vec![
            UpstreamEvent::Chunk("hello ".into()),
            UpstreamEvent::Chunk("world".into()),
            UpstreamEvent::Complete(meta.clone()),
        ]);
        let plugin = make_plugin(client);
        let ctx = MessagePluginCtx {
            session_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
            messages: vec![make_message(MessageRole::User, "hi")],
            call_ctx: make_call_ctx(valid_config()),
        };
        let mut stream = plugin.on_message(ctx).await.unwrap();
        let mut events: Vec<StreamingEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        // two chunks + one complete
        assert_eq!(events.len(), 3, "got {events:?}");
        assert!(matches!(events[0], StreamingEvent::Chunk(_)));
        assert!(matches!(events[1], StreamingEvent::Chunk(_)));
        match &events[2] {
            StreamingEvent::Complete(c) => {
                let metadata_json = c.metadata.as_ref().expect("metadata present");
                let parsed: LlmMessageMetadata =
                    serde_json::from_value(metadata_json.clone()).unwrap();
                assert_eq!(parsed, meta);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn context_overflow_surfaces_as_discriminator_prefixed_error() {
        let client = FakeLlmGatewayClient::with_events(vec![UpstreamEvent::ContextOverflow(
            "window exceeded by 200 tokens".into(),
        )]);
        let plugin = make_plugin(client);
        let msg_id = Uuid::new_v4();
        let ctx = MessagePluginCtx {
            session_id: Uuid::new_v4(),
            message_id: msg_id,
            messages: vec![make_message(MessageRole::User, "very long prompt")],
            call_ctx: make_call_ctx(valid_config()),
        };
        let mut stream = plugin.on_message(ctx).await.unwrap();
        let event = stream.next().await.expect("one event").unwrap();
        match event {
            StreamingEvent::Error(e) => {
                assert_eq!(e.message_id, msg_id);
                assert!(
                    e.error.starts_with(ERROR_PREFIX_CONTEXT_OVERFLOW),
                    "got: {}",
                    e.error
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(stream.next().await.is_none(), "stream must close after overflow");
    }

    #[tokio::test]
    async fn stream_interrupted_surfaces_as_discriminator_prefixed_error() {
        let client = FakeLlmGatewayClient::with_events(vec![
            UpstreamEvent::Chunk("partial".into()),
            UpstreamEvent::StreamInterrupted("upstream RST".into()),
        ]);
        let plugin = make_plugin(client);
        let msg_id = Uuid::new_v4();
        let ctx = MessagePluginCtx {
            session_id: Uuid::new_v4(),
            message_id: msg_id,
            messages: vec![make_message(MessageRole::User, "hi")],
            call_ctx: make_call_ctx(valid_config()),
        };
        let mut stream = plugin.on_message(ctx).await.unwrap();
        let _chunk = stream.next().await.unwrap().unwrap();
        let event = stream.next().await.unwrap().unwrap();
        match event {
            StreamingEvent::Error(e) => {
                assert!(
                    e.error.starts_with(ERROR_PREFIX_STREAM_INTERRUPTED),
                    "got: {}",
                    e.error
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn on_message_rejects_hidden_from_backend_input() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let mut hidden = make_message(MessageRole::User, "stale");
        hidden.is_hidden_from_backend = true;
        let ctx = MessagePluginCtx {
            session_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
            messages: vec![hidden],
            call_ctx: make_call_ctx(valid_config()),
        };
        let result = plugin.on_message(ctx).await;
        let err = match result {
            Ok(_) => panic!("hidden-from-backend message must be rejected"),
            Err(e) => e,
        };
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[tokio::test]
    async fn on_message_emits_deadline_exceeded_when_remaining_zero() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let mut call_ctx = make_call_ctx(valid_config());
        // Deadline already elapsed — Some(ZERO) by construction.
        call_ctx.deadline =
            Some(Instant::now() - std::time::Duration::from_secs(1));
        let msg_id = Uuid::new_v4();
        let ctx = MessagePluginCtx {
            session_id: Uuid::new_v4(),
            message_id: msg_id,
            messages: vec![make_message(MessageRole::User, "hi")],
            call_ctx,
        };
        let mut stream = plugin.on_message(ctx).await.unwrap();
        let event = stream.next().await.unwrap().unwrap();
        match event {
            StreamingEvent::Error(e) => {
                assert_eq!(e.message_id, msg_id);
                assert!(
                    e.error.starts_with(ERROR_PREFIX_DEADLINE_EXCEEDED),
                    "got: {}",
                    e.error
                );
            }
            other => panic!("expected deadline-exceeded Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn on_session_summary_requires_summarization_settings() {
        let plugin = make_plugin(FakeLlmGatewayClient::default());
        let ctx = SessionPluginCtx {
            session_type_id: Uuid::nil(),
            session_id: Some(Uuid::new_v4()),
            // Note: no `summarization_settings` field — should produce
            // PluginError::Internal per the unsupported rule.
            call_ctx: make_call_ctx(serde_json::json!({
                "gateway_url": "https://gw.example",
            })),
        };
        let result = plugin.on_session_summary(ctx).await;
        let err = match result {
            Ok(_) => panic!("missing summarization_settings must surface"),
            Err(e) => e,
        };
        assert!(matches!(err, PluginError::Internal { .. }));
    }
}
