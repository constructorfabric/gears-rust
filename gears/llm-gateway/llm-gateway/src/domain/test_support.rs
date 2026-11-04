//! Shared fakes and builders for domain, infra and REST unit tests.
//!
//! Every helper is test-only, so `expect` on internal mutexes is fine.
#![allow(clippy::missing_panics_doc)]

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use llm_gateway_sdk::models::core::{
    ReasoningConfig, ResponseError, ResponseStatus, TextFormat, ToolChoice, ToolChoiceMode,
    TruncationStrategy,
};
use llm_gateway_sdk::models::streaming::{ErrorEvent, OutputTextEvent, ResponseSnapshotEvent};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, MediaInputMode, ProviderCallCtx, ProviderPluginCapabilities,
    ResponseEventStream, ResponseInput, ResponseResource, StreamingEvent, Usage,
};
use model_registry_sdk::{ModelInfoV1, ModelV1};
use secrecy::SecretString;
use tokio::sync::Notify;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::Service;
use super::ports::{
    CredentialBroker, ModelResolver, PluginDirectory, ResolvedPlugin, ToolSchemaSource,
};
use super::provider_rules::{ProviderRules, ProviderRulesRegistry};
use crate::config::{LimitsConfig, StreamConfig};

/// Provider identity used by the sample model and plugin.
pub const OPENAI_TYPE: &str = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~";
/// Secret value used to prove keys never leak into `Debug` output.
pub const SECRET_VALUE: &str = "sk-test-very-secret-0123456789";

/// Authenticated caller context.
#[must_use]
pub fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .subject_type("user")
        .build()
        .expect("authenticated context")
}

/// Model info of provider type `gts_type`, built from its wire shape because
/// `ModelInfoV1` is `#[non_exhaustive]`.
#[must_use]
pub fn sample_model_info(gts_type: &str) -> ModelInfoV1 {
    sample_model_info_patched(gts_type, |_| {})
}

/// [`sample_model_info`] with `patch` applied to the wire JSON first (e.g.
/// `|v| v["capabilities"]["function_calling"] = json!(true)`).
#[must_use]
pub fn sample_model_info_patched(
    gts_type: &str,
    patch: impl FnOnce(&mut serde_json::Value),
) -> ModelInfoV1 {
    let media = serde_json::json!({ "enabled": false, "supported_mime_types": [] });
    let mut value = serde_json::json!({
        "gts_type": gts_type,
        "display_name": "Sample",
        "description": null,
        "family": null,
        "vendor": null,
        "managed": false,
        "architecture": null,
        "size_bytes": null,
        "format": null,
        "region": null,
        "hosted_by": null,
        "last_release_at": null,
        "reasoning_level": null,
        "version": null,
        "sort_order": null,
        "icon": null,
        "multiplier_display": null,
        "performance": { "response_latency_ms": null, "tokens_per_second": null },
        "additional_info": {},
        "supported_api": ["completion"],
        "provider_model_id": "gpt-4o",
        "capabilities": {
            "vision": media,
            "reasoning": { "effort": false, "toggle": false, "resume": false, "budget": false },
            "function_calling": false,
            "response_schema": false,
            "streaming": false,
            "file_input": media,
            "image_generation": media,
            "audio_input": media,
            "audio_output": media,
            "code_interpreter": false,
            "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false }
        },
        "disabled_capabilities": model_registry_sdk::DisabledCapabilities::default(),
        "context_window": { "max_input_tokens": 8192, "max_output_tokens": 4096, "output_vector_size": null },
        "default_parameters": model_registry_sdk::DefaultInferenceParametersV1::default(),
        "allow_parameter_override": false,
        "allow_extra_params": [],
        "provider_settings": { "oagw_alias": "openai-prod" }
    });
    patch(&mut value);
    serde_json::from_value(value).expect("sample model info deserializes")
}

/// Approved production model of provider type `gts_type`.
#[must_use]
pub fn sample_model(gts_type: &str) -> ModelV1 {
    let value = serde_json::json!({
        "id": Uuid::from_u128(9),
        "canonical_id": "openai::gpt-4o",
        "lifecycle_status": "production",
        "approval_status": "approved",
        "info": sample_model_info(gts_type),
    });
    serde_json::from_value(value).expect("sample model deserializes")
}

/// Minimal create-response body for `model` with a text prompt.
#[must_use]
pub fn body(model: &str) -> CreateResponseBody {
    CreateResponseBody {
        model: model.to_owned(),
        input: Some(ResponseInput::Text("hello".to_owned())),
        ..CreateResponseBody::default()
    }
}

/// Completed response resource with `id` for `model`.
#[must_use]
pub fn sample_response(id: &str, model: &str) -> ResponseResource {
    ResponseResource {
        id: id.to_owned(),
        object: "response".to_owned(),
        created_at: 0,
        completed_at: Some(1),
        status: ResponseStatus::Completed,
        incomplete_details: None,
        model: model.to_owned(),
        instructions: None,
        output: Vec::new(),
        error: None,
        tools: Vec::new(),
        tool_choice: ToolChoice::Mode(ToolChoiceMode::Auto),
        truncation: TruncationStrategy::Disabled,
        parallel_tool_calls: false,
        text: TextFormat::default(),
        top_p: 1.0,
        presence_penalty: 0.0,
        frequency_penalty: 0.0,
        top_logprobs: 0,
        temperature: 1.0,
        reasoning: ReasoningConfig::default(),
        usage: None,
        max_output_tokens: None,
        max_tool_calls: None,
        service_tier: "default".to_owned(),
        metadata: None,
        safety_identifier: None,
        prompt_cache_key: None,
        ignored_parameters: Vec::new(),
        provider_details: None,
    }
}

/// Completed response whose single assistant message carries `text` as
/// `output_text`.
#[must_use]
pub fn sample_response_with_text(id: &str, model: &str, text: &str) -> ResponseResource {
    let mut resource = sample_response(id, model);
    resource.output = serde_json::from_value(serde_json::json!([{
        "type": "message",
        "id": "msg_1",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    }]))
    .expect("output message deserializes");
    resource
}

// ── Streaming event builders ────────────────────────────────────────────────

/// Snapshot of response `id` with `status` (and `usage` when given).
#[must_use]
pub fn snapshot(id: &str, status: ResponseStatus, usage: Option<Usage>) -> ResponseResource {
    let mut resource = sample_response(id, "gpt-4o");
    resource.status = status;
    resource.usage = usage;
    resource
}

/// `response.created` carrying a provider-side `id` and `seq`.
#[must_use]
pub fn ev_created(seq: u64, id: &str) -> StreamingEvent {
    StreamingEvent::Created(ResponseSnapshotEvent {
        sequence_number: seq,
        response: snapshot(id, ResponseStatus::InProgress, None),
    })
}

/// `response.in_progress`, optionally carrying provider usage.
#[must_use]
pub fn ev_in_progress(seq: u64, id: &str, usage: Option<Usage>) -> StreamingEvent {
    StreamingEvent::InProgress(ResponseSnapshotEvent {
        sequence_number: seq,
        response: snapshot(id, ResponseStatus::InProgress, usage),
    })
}

/// `response.output_text.delta` with `text`.
#[must_use]
pub fn ev_text_delta(seq: u64, text: &str) -> StreamingEvent {
    StreamingEvent::OutputTextDelta(OutputTextEvent {
        sequence_number: seq,
        item_id: "msg_1".to_owned(),
        output_index: 0,
        content_index: 0,
        text: text.to_owned(),
        logprobs: None,
    })
}

/// `response.completed` with a provider-side `id` and optional usage.
#[must_use]
pub fn ev_completed(seq: u64, id: &str, usage: Option<Usage>) -> StreamingEvent {
    StreamingEvent::Completed(ResponseSnapshotEvent {
        sequence_number: seq,
        response: snapshot(id, ResponseStatus::Completed, usage),
    })
}

/// `response.incomplete` with a provider-side `id`.
#[must_use]
pub fn ev_incomplete(seq: u64, id: &str) -> StreamingEvent {
    StreamingEvent::Incomplete(ResponseSnapshotEvent {
        sequence_number: seq,
        response: snapshot(id, ResponseStatus::Incomplete, None),
    })
}

/// Provider-sent `response.failed` with error `code`.
#[must_use]
pub fn ev_failed(seq: u64, id: &str, code: &str) -> StreamingEvent {
    let mut response = snapshot(id, ResponseStatus::Failed, None);
    response.error = Some(ResponseError::new(code, "provider failure"));
    StreamingEvent::Failed(ResponseSnapshotEvent {
        sequence_number: seq,
        response,
    })
}

/// Provider-sent standalone `error` event with `code`.
#[must_use]
pub fn ev_error(seq: u64, code: &str) -> StreamingEvent {
    StreamingEvent::Error(ErrorEvent {
        sequence_number: seq,
        error: ResponseError::new(code, "provider error"),
    })
}

/// Ordered log of dependency calls, shared by all fakes of one harness.
#[derive(Debug, Default)]
pub struct CallLog(Mutex<Vec<&'static str>>);

impl CallLog {
    fn push(&self, name: &'static str) {
        self.0.lock().expect("call log").push(name);
    }

    /// Snapshot of the recorded calls in order.
    #[must_use]
    pub fn entries(&self) -> Vec<&'static str> {
        self.0.lock().expect("call log").clone()
    }
}

// ── PDP fake ────────────────────────────────────────────────────────────────

/// How the PDP fake answers.
#[derive(Debug, Clone, Copy)]
pub enum PdpMode {
    /// Permit, scoped to the request's `OWNER_TENANT_ID`.
    Permit,
    /// Deny every request.
    Deny,
    /// Fail the evaluation transport.
    Unreachable,
}

/// Counting PDP fake.
pub struct FakePdp {
    mode: PdpMode,
    calls: AtomicUsize,
    log: Arc<CallLog>,
    last_request: Mutex<Option<EvaluationRequest>>,
}

impl FakePdp {
    /// Build a PDP fake in `mode`.
    #[must_use]
    pub fn new(mode: PdpMode, log: Arc<CallLog>) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicUsize::new(0),
            log,
            last_request: Mutex::new(None),
        })
    }

    /// Number of `evaluate` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Most recent request the PEP composed.
    #[must_use]
    pub fn last_request(&self) -> Option<EvaluationRequest> {
        self.last_request.lock().expect("pdp request").clone()
    }
}

#[async_trait]
impl AuthZResolverApi for FakePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("authz");
        let tenant = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| Uuid::from_u128(2).to_string(), ToOwned::to_owned);
        *self.last_request.lock().expect("pdp request") = Some(request);
        match self.mode {
            PdpMode::Permit => Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::Eq(EqPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            tenant,
                        ))],
                    }],
                    deny_reason: None,
                },
            }),
            PdpMode::Deny => Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            }),
            PdpMode::Unreachable => Err(CanonicalError::service_unavailable()
                .with_detail("test fake: authz transport failure".to_owned())
                .create()),
        }
    }
}

// ── Model resolver fake ─────────────────────────────────────────────────────

type ModelOutcome = Box<dyn Fn() -> Result<ModelInfoV1, LlmGatewayError> + Send + Sync>;

/// Model resolver fake with a call counter.
pub struct FakeModels {
    outcome: ModelOutcome,
    calls: AtomicUsize,
    requested: Mutex<Vec<String>>,
    log: Arc<CallLog>,
}

impl FakeModels {
    /// Resolver that always finds `info`.
    #[must_use]
    pub fn found(info: ModelInfoV1, log: Arc<CallLog>) -> Arc<Self> {
        Self::with(move || Ok(info.clone()), log)
    }

    /// Resolver driven by `outcome`.
    #[must_use]
    pub fn with(
        outcome: impl Fn() -> Result<ModelInfoV1, LlmGatewayError> + Send + Sync + 'static,
        log: Arc<CallLog>,
    ) -> Arc<Self> {
        Arc::new(Self {
            outcome: Box::new(outcome),
            calls: AtomicUsize::new(0),
            requested: Mutex::new(Vec::new()),
            log,
        })
    }

    /// Number of `resolve` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Model identifiers asked for so far.
    #[must_use]
    pub fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("requested").clone()
    }
}

#[async_trait]
impl ModelResolver for FakeModels {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        model: &str,
    ) -> Result<ModelInfoV1, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("models");
        self.requested
            .lock()
            .expect("requested")
            .push(model.to_owned());
        (self.outcome)()
    }
}

// ── Provider plugin fake ────────────────────────────────────────────────────

type PluginOutcome =
    Box<dyn Fn(&ProviderCallCtx) -> Result<ResponseResource, LlmGatewayError> + Send + Sync>;

/// Provider plugin fake: records each call context, optionally parks inside
/// the call until released.
pub struct FakePlugin {
    outcome: PluginOutcome,
    calls: AtomicUsize,
    contexts: Mutex<Vec<ProviderCallCtx>>,
    bodies: Mutex<Vec<CreateResponseBody>>,
    web_search: AtomicBool,
    log: Arc<CallLog>,
    /// Notified when a call has entered the plugin (only when `gated`).
    pub entered: Notify,
    /// Releases a parked call (only when `gated`).
    pub release: Notify,
    gated: bool,
    streaming_transport: AtomicBool,
    script: Mutex<StreamScript>,
    stream_calls: AtomicUsize,
    stream_dropped: Arc<AtomicBool>,
    stream_dropped_notify: Arc<Notify>,
}

/// What the fake plugin's `create_response_stream` does.
#[derive(Clone)]
pub enum StreamScript {
    /// Yield the events, then end the stream normally.
    Events(Vec<StreamingEvent>),
    /// Yield the events, then stay pending until dropped.
    EventsThenHang(Vec<StreamingEvent>),
    /// Fail before the stream starts.
    StartError(fn() -> LlmGatewayError),
}

/// Stream wrapper that records its own drop (the plugin side of upstream
/// cancellation).
struct TrackedStream {
    inner: ResponseEventStream,
    dropped: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl futures_core::Stream for TrackedStream {
    type Item = StreamingEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Drop for TrackedStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

impl FakePlugin {
    /// Plugin that answers with a completed response carrying
    /// `ctx.request_id` as its `id`.
    #[must_use]
    pub fn echo_id(log: Arc<CallLog>) -> Arc<Self> {
        Self::with(|c| Ok(sample_response(&c.request_id, "gpt-4o")), log, false)
    }

    /// Plugin driven by `outcome`; `gated` parks every call until
    /// `release` is notified.
    #[must_use]
    pub fn with(
        outcome: impl Fn(&ProviderCallCtx) -> Result<ResponseResource, LlmGatewayError>
        + Send
        + Sync
        + 'static,
        log: Arc<CallLog>,
        gated: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            outcome: Box::new(outcome),
            calls: AtomicUsize::new(0),
            contexts: Mutex::new(Vec::new()),
            bodies: Mutex::new(Vec::new()),
            web_search: AtomicBool::new(false),
            log,
            entered: Notify::new(),
            release: Notify::new(),
            gated,
            streaming_transport: AtomicBool::new(false),
            script: Mutex::new(StreamScript::Events(Vec::new())),
            stream_calls: AtomicUsize::new(0),
            stream_dropped: Arc::new(AtomicBool::new(false)),
            stream_dropped_notify: Arc::new(Notify::new()),
        })
    }

    /// Number of `create_response` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Report (or stop reporting) `streaming_transport`.
    pub fn set_streaming_transport(&self, enabled: bool) {
        self.streaming_transport.store(enabled, Ordering::SeqCst);
    }

    /// Script the next `create_response_stream` calls.
    pub fn set_stream(&self, script: StreamScript) {
        *self.script.lock().expect("script") = script;
    }

    /// Number of `create_response_stream` calls.
    #[must_use]
    pub fn stream_calls(&self) -> usize {
        self.stream_calls.load(Ordering::SeqCst)
    }

    /// Whether the stream handed out by the plugin has been dropped.
    #[must_use]
    pub fn stream_dropped(&self) -> bool {
        self.stream_dropped.load(Ordering::SeqCst)
    }

    /// Wait (bounded) until the stream handed out by the plugin is dropped.
    ///
    /// # Panics
    ///
    /// When it is not dropped within two seconds.
    pub async fn wait_stream_dropped(&self) {
        let notified = self.stream_dropped_notify.notified();
        if self.stream_dropped() {
            return;
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), notified)
            .await
            .expect("the plugin stream was not dropped in time");
    }

    /// Report (or stop reporting) the `web_search` integration capability.
    pub fn set_web_search(&self, enabled: bool) {
        self.web_search.store(enabled, Ordering::SeqCst);
    }

    /// Request bodies received so far (the effective requests).
    #[must_use]
    pub fn bodies(&self) -> Vec<CreateResponseBody> {
        self.bodies.lock().expect("bodies").clone()
    }

    /// Provider call contexts received so far.
    #[must_use]
    pub fn contexts(&self) -> Vec<ProviderCallCtx> {
        self.contexts.lock().expect("contexts").clone()
    }
}

#[async_trait]
impl LlmGatewayProviderPluginClientV1 for FakePlugin {
    fn capabilities(&self) -> ProviderPluginCapabilities {
        let mut caps = ProviderPluginCapabilities::new(MediaInputMode::UrlForward);
        if self.streaming_transport.load(Ordering::SeqCst) {
            caps = caps.with_streaming_transport();
        }
        if self.web_search.load(Ordering::SeqCst) {
            caps.with_web_search()
        } else {
            caps
        }
    }

    async fn create_response(
        &self,
        _ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugin");
        self.bodies.lock().expect("bodies").push(body);
        self.contexts.lock().expect("contexts").push(call.clone());
        if self.gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        (self.outcome)(call)
    }

    async fn create_response_stream(
        &self,
        _ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugin");
        self.bodies.lock().expect("bodies").push(body);
        self.contexts.lock().expect("contexts").push(call.clone());
        let script = self.script.lock().expect("script").clone();
        let inner: ResponseEventStream = match script {
            StreamScript::StartError(make) => return Err(make()),
            StreamScript::Events(events) => Box::pin(futures_util::stream::iter(events)),
            StreamScript::EventsThenHang(events) => Box::pin(futures_util::StreamExt::chain(
                futures_util::stream::iter(events),
                futures_util::stream::pending(),
            )),
        };
        Ok(Box::pin(TrackedStream {
            inner,
            dropped: Arc::clone(&self.stream_dropped),
            notify: Arc::clone(&self.stream_dropped_notify),
        }))
    }

    async fn create_embedding(
        &self,
        _ctx: &SecurityContext,
        _call: &ProviderCallCtx,
        _req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        Err(LlmGatewayError::internal("test fake: no embeddings"))
    }
}

// ── Plugin directory fake ───────────────────────────────────────────────────

/// Plugin directory fake serving one plugin for one provider type.
pub struct FakePluginDirectory {
    provider_type: String,
    plugin: Arc<FakePlugin>,
    tool_namespace: String,
    calls: AtomicUsize,
    requested: Mutex<Vec<String>>,
    log: Arc<CallLog>,
}

impl FakePluginDirectory {
    /// Directory that resolves `plugin` for `provider_type` only.
    #[must_use]
    pub fn new(provider_type: &str, plugin: Arc<FakePlugin>, log: Arc<CallLog>) -> Arc<Self> {
        Arc::new(Self {
            provider_type: provider_type.to_owned(),
            plugin,
            tool_namespace: "openai".to_owned(),
            calls: AtomicUsize::new(0),
            requested: Mutex::new(Vec::new()),
            log,
        })
    }

    /// Same directory, resolving plugins with `tool_namespace` (default
    /// `openai`).
    #[must_use]
    pub fn with_tool_namespace(
        provider_type: &str,
        plugin: Arc<FakePlugin>,
        tool_namespace: &str,
        log: Arc<CallLog>,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider_type: provider_type.to_owned(),
            plugin,
            tool_namespace: tool_namespace.to_owned(),
            calls: AtomicUsize::new(0),
            requested: Mutex::new(Vec::new()),
            log,
        })
    }

    /// Number of `resolve` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Provider types asked for so far.
    #[must_use]
    pub fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("requested").clone()
    }
}

#[async_trait]
impl PluginDirectory for FakePluginDirectory {
    async fn resolve(&self, provider_type: &str) -> Result<ResolvedPlugin, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugins");
        self.requested
            .lock()
            .expect("requested")
            .push(provider_type.to_owned());
        if provider_type == self.provider_type {
            Ok(ResolvedPlugin::new(
                self.plugin.clone(),
                self.tool_namespace.clone(),
            ))
        } else {
            Err(LlmGatewayError::provider_not_supported(provider_type))
        }
    }
}

// ── Credential broker fake ──────────────────────────────────────────────────

type BrokerOutcome = Box<dyn Fn() -> Result<Option<SecretString>, LlmGatewayError> + Send + Sync>;

/// Credential broker fake with a call counter.
pub struct FakeBroker {
    outcome: BrokerOutcome,
    calls: AtomicUsize,
    log: Arc<CallLog>,
}

impl FakeBroker {
    /// Broker driven by `outcome`.
    #[must_use]
    pub fn with(
        outcome: impl Fn() -> Result<Option<SecretString>, LlmGatewayError> + Send + Sync + 'static,
        log: Arc<CallLog>,
    ) -> Arc<Self> {
        Arc::new(Self {
            outcome: Box::new(outcome),
            calls: AtomicUsize::new(0),
            log,
        })
    }

    /// Number of `resolve` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CredentialBroker for FakeBroker {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _model: &ModelInfoV1,
    ) -> Result<Option<SecretString>, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("credentials");
        (self.outcome)()
    }
}

// ── Tool schema source fake ─────────────────────────────────────────────────

/// Tool schema source fake: serves schemas by GTS id, records requests and
/// the caller subject of each.
pub struct FakeToolSchemas {
    schemas: Mutex<HashMap<String, serde_json::Value>>,
    requested: Mutex<Vec<String>>,
    subjects: Mutex<Vec<Uuid>>,
    log: Arc<CallLog>,
}

impl FakeToolSchemas {
    /// Source with no schemas: every id is `schema_not_found`.
    #[must_use]
    pub fn new(log: Arc<CallLog>) -> Arc<Self> {
        Arc::new(Self {
            schemas: Mutex::new(HashMap::new()),
            requested: Mutex::new(Vec::new()),
            subjects: Mutex::new(Vec::new()),
            log,
        })
    }

    /// Serve `schema` under `schema_id` (no `gts://` prefix).
    pub fn insert(&self, schema_id: &str, schema: serde_json::Value) {
        self.schemas
            .lock()
            .expect("schemas")
            .insert(schema_id.to_owned(), schema);
    }

    /// Number of `schema` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.requested.lock().expect("requested").len()
    }

    /// Schema ids asked for so far, in order.
    #[must_use]
    pub fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("requested").clone()
    }

    /// Subject ids of the contexts the schemas were requested under.
    #[must_use]
    pub fn subjects(&self) -> Vec<Uuid> {
        self.subjects.lock().expect("subjects").clone()
    }
}

#[async_trait]
impl ToolSchemaSource for FakeToolSchemas {
    async fn schema(
        &self,
        ctx: &SecurityContext,
        schema_id: &str,
    ) -> Result<serde_json::Value, LlmGatewayError> {
        self.log.push("tool_schemas");
        self.requested
            .lock()
            .expect("requested")
            .push(schema_id.to_owned());
        self.subjects
            .lock()
            .expect("subjects")
            .push(ctx.subject_id());
        self.schemas
            .lock()
            .expect("schemas")
            .get(schema_id)
            .cloned()
            .ok_or_else(|| LlmGatewayError::schema_not_found(schema_id))
    }
}

// ── Provider rules fake ─────────────────────────────────────────────────────

/// Provider rules driven by plain sets; the default supports everything.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_field_names)]
pub struct FakeRules {
    /// Generic parameters the rules reject.
    pub unsupported_params: BTreeSet<&'static str>,
    /// Cache-control hints the rules reject.
    pub unsupported_hints: BTreeSet<&'static str>,
    /// Construct reported for every schema, when set.
    pub unsupported_construct: Option<&'static str>,
}

impl ProviderRules for FakeRules {
    fn supports_parameter(&self, _model: &ModelInfoV1, parameter: &str) -> bool {
        !self.unsupported_params.contains(parameter)
    }

    fn supports_cache_control(&self, _model: &ModelInfoV1, hint: &str) -> bool {
        !self.unsupported_hints.contains(hint)
    }

    fn unsupported_schema_construct(
        &self,
        _model: &ModelInfoV1,
        _schema: &serde_json::Value,
    ) -> Option<String> {
        self.unsupported_construct.map(ToOwned::to_owned)
    }
}

// ── Harness ─────────────────────────────────────────────────────────────────

/// All fakes of one pipeline; mutate the public fields, then call
/// [`Harness::service`].
pub struct Harness {
    pub log: Arc<CallLog>,
    pub pdp: Arc<FakePdp>,
    pub models: Arc<FakeModels>,
    pub plugin: Arc<FakePlugin>,
    pub plugins: Arc<FakePluginDirectory>,
    pub broker: Arc<FakeBroker>,
    pub tool_schemas: Arc<FakeToolSchemas>,
    /// Rules registered for [`OPENAI_TYPE`]; supports everything by default.
    pub rules: FakeRules,
    pub limits: LimitsConfig,
    pub stream: StreamConfig,
}

impl Harness {
    /// Happy path: permit, model of [`OPENAI_TYPE`] found, plugin echoes the
    /// response id, no key.
    #[must_use]
    pub fn new() -> Self {
        let log = Arc::new(CallLog::default());
        let plugin = FakePlugin::echo_id(log.clone());
        Self {
            log: log.clone(),
            pdp: FakePdp::new(PdpMode::Permit, log.clone()),
            models: FakeModels::found(sample_model_info(OPENAI_TYPE), log.clone()),
            plugin: plugin.clone(),
            plugins: FakePluginDirectory::new(OPENAI_TYPE, plugin, log.clone()),
            broker: FakeBroker::with(|| Ok(None), log.clone()),
            tool_schemas: FakeToolSchemas::new(log),
            rules: FakeRules::default(),
            limits: LimitsConfig::default(),
            stream: StreamConfig::default(),
        }
    }

    /// Replace the plugin (and the directory that serves it).
    pub fn set_plugin(&mut self, plugin: Arc<FakePlugin>) {
        self.plugins = FakePluginDirectory::new(OPENAI_TYPE, plugin.clone(), self.log.clone());
        self.plugin = plugin;
    }

    /// Make the directory resolve the plugin with `tool_namespace`.
    pub fn set_tool_namespace(&mut self, tool_namespace: &str) {
        self.plugins = FakePluginDirectory::with_tool_namespace(
            OPENAI_TYPE,
            self.plugin.clone(),
            tool_namespace,
            self.log.clone(),
        );
    }

    /// Serve a model that supports streaming and a plugin with streaming
    /// transport, so `create_response_stream` passes the capability gate.
    pub fn enable_streaming(&mut self) {
        self.enable_streaming_with(|_| {});
    }

    /// [`Self::enable_streaming`] with an extra patch on the model's wire JSON.
    pub fn enable_streaming_with(&mut self, patch: impl FnOnce(&mut serde_json::Value)) {
        self.models = FakeModels::found(
            sample_model_info_patched(OPENAI_TYPE, |v| {
                v["capabilities"]["streaming"] = serde_json::json!(true);
                patch(v);
            }),
            self.log.clone(),
        );
        self.plugin.set_streaming_transport(true);
    }

    /// Build the service over the current fakes.
    #[must_use]
    pub fn service(&self) -> Arc<Service> {
        Arc::new(Service::new(
            PolicyEnforcer::new(self.pdp.clone()),
            self.models.clone(),
            self.plugins.clone(),
            self.broker.clone(),
            self.tool_schemas.clone(),
            Arc::new(
                ProviderRulesRegistry::permissive()
                    .with_rules(OPENAI_TYPE, Arc::new(self.rules.clone())),
            ),
            &self.limits,
            &self.stream,
        ))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
