//! Shared fakes and builders for domain, infra and REST unit tests.
//!
//! Every helper is test-only, so `expect` on internal mutexes is fine.
#![allow(clippy::missing_panics_doc)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
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
use llm_gateway_sdk::models::streaming::{
    ErrorEvent, OutputTextEvent, ReasoningSummaryTextEvent, ResponseSnapshotEvent,
};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingData, EmbeddingRequest, EmbeddingResponse, EmbeddingVector,
    LlmGatewayError, LlmGatewayProviderPluginClientV1, MediaInputMode, ProviderCallCtx,
    ProviderPluginCapabilities, ResponseEventStream, ResponseInput, ResponseResource,
    StreamingEvent, Usage,
};
use model_registry_sdk::{ModelInfoV1, ModelV1};
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, BatchDebitRequest, BatchDecision, CommitLeaseRequest,
    DebitRequest, Decision, DecisionPreview, DecisionResult, LeaseToken, PageResult,
    PreviewRequest, QuotaEnforcementClientV1, QuotaEnforcementError, QuotaId, QuotaSnapshot,
    ReleaseLeaseRequest, RollbackRequest, SnapshotRequest,
};
use secrecy::SecretString;
use tokio::sync::Notify;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use usage_collector_sdk::{
    AggregationResult, AggregationSpec, CreateUsageRecord, MetadataFilter, UsageCollectorClientV1,
    UsageCollectorError, UsageRecord, UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

use super::Service;
use super::ports::{
    CredentialBroker, IdentityContext, ModelResolver, PluginDirectory, ResolvedPlugin,
    ServiceIdentity, ToolSchemaSource, UsageReport, UsageSink,
};
use super::pricing::ModelPricing;
use super::provider_rules::{PermissiveRules, ProviderRules, ProviderRulesRegistry};
use super::quota_guard::QuotaGuard;
use crate::config::{LimitsConfig, QuotaConfig, StreamConfig, TimeoutsConfig};

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
        // Zero pricing by default: admitted without a lease, so tests that
        // are not about quota never touch Quota Enforcement. Quota tests
        // patch the rates with `set_cost`.
        "provider_settings": {
            "oagw_alias": "openai-prod",
            "cost": { "input_per_1k_micro": 0, "output_per_1k_micro": 0 }
        }
    });
    patch(&mut value);
    serde_json::from_value(value).expect("sample model info deserializes")
}

/// Patch for [`sample_model_info_patched`]: set `provider_settings.cost`
/// rates (`None` is a JSON `null`, i.e. a missing rate).
pub fn set_cost(value: &mut serde_json::Value, input: Option<u64>, output: Option<u64>) {
    value["provider_settings"]["cost"] = serde_json::json!({
        "input_per_1k_micro": input,
        "output_per_1k_micro": output,
    });
}

/// Patch for [`sample_model_info_patched`]: drop `provider_settings.cost`.
pub fn remove_cost(value: &mut serde_json::Value) {
    value["provider_settings"]
        .as_object_mut()
        .expect("provider_settings is an object")
        .remove("cost");
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

/// Embedding response `id` for `model` with one two-component vector.
#[must_use]
pub fn sample_embedding_response(id: &str, model: &str) -> EmbeddingResponse {
    EmbeddingResponse::new(
        id,
        model,
        vec![EmbeddingData {
            index: 0,
            embedding: EmbeddingVector::Floats(vec![0.5, 0.25]),
        }],
        Usage::unknown(),
    )
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

/// `response.reasoning_summary_text.delta` with `text`.
#[must_use]
pub fn ev_summary_delta(seq: u64, text: &str) -> StreamingEvent {
    StreamingEvent::ReasoningSummaryTextDelta(ReasoningSummaryTextEvent {
        sequence_number: seq,
        item_id: "rs_1".to_owned(),
        output_index: 0,
        summary_index: 0,
        text: text.to_owned(),
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

type EmbeddingOutcome =
    Box<dyn Fn(&ProviderCallCtx) -> Result<EmbeddingResponse, LlmGatewayError> + Send + Sync>;

type PluginOutcome =
    Box<dyn Fn(&ProviderCallCtx) -> Result<ResponseResource, LlmGatewayError> + Send + Sync>;

/// Provider plugin fake: records each call context, optionally parks inside
/// the call until released.
pub struct FakePlugin {
    outcome: PluginOutcome,
    calls: AtomicUsize,
    contexts: Mutex<Vec<ProviderCallCtx>>,
    bodies: Mutex<Vec<CreateResponseBody>>,
    embedding_outcome: Mutex<EmbeddingOutcome>,
    embedding_calls: AtomicUsize,
    embedding_requests: Mutex<Vec<EmbeddingRequest>>,
    embedding_contexts: Mutex<Vec<ProviderCallCtx>>,
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
            embedding_outcome: Mutex::new(Box::new(|c| {
                Ok(sample_embedding_response(&c.request_id, "text-embedding"))
            })),
            embedding_calls: AtomicUsize::new(0),
            embedding_requests: Mutex::new(Vec::new()),
            embedding_contexts: Mutex::new(Vec::new()),
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

    /// Script the `create_embedding` outcome (default: one vector echoing
    /// `ctx.request_id` as the response `id`).
    pub fn set_embedding(
        &self,
        outcome: impl Fn(&ProviderCallCtx) -> Result<EmbeddingResponse, LlmGatewayError>
        + Send
        + Sync
        + 'static,
    ) {
        *self.embedding_outcome.lock().expect("embedding outcome") = Box::new(outcome);
    }

    /// Number of `create_embedding` calls.
    #[must_use]
    pub fn embedding_calls(&self) -> usize {
        self.embedding_calls.load(Ordering::SeqCst)
    }

    /// Embedding requests received so far (the effective requests).
    #[must_use]
    pub fn embedding_requests(&self) -> Vec<EmbeddingRequest> {
        self.embedding_requests
            .lock()
            .expect("embedding requests")
            .clone()
    }

    /// Provider call contexts of the embedding calls received so far.
    #[must_use]
    pub fn embedding_contexts(&self) -> Vec<ProviderCallCtx> {
        self.embedding_contexts
            .lock()
            .expect("embedding contexts")
            .clone()
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
        call: &ProviderCallCtx,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        self.embedding_calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugin");
        self.embedding_requests
            .lock()
            .expect("embedding requests")
            .push(req);
        self.embedding_contexts
            .lock()
            .expect("embedding contexts")
            .push(call.clone());
        if self.gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        (self.embedding_outcome.lock().expect("embedding outcome"))(call)
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
    /// Reject every `dimensions` value.
    pub reject_all_dimensions: bool,
}

impl ProviderRules for FakeRules {
    fn supports_parameter(&self, _model: &ModelInfoV1, parameter: &str) -> bool {
        !self.unsupported_params.contains(parameter)
    }

    fn supports_cache_control(&self, _model: &ModelInfoV1, hint: &str) -> bool {
        !self.unsupported_hints.contains(hint)
    }

    fn supports_dimensions(&self, _model: &ModelInfoV1, _dimensions: u32) -> bool {
        !self.reject_all_dimensions
    }

    fn unsupported_schema_construct(
        &self,
        _model: &ModelInfoV1,
        _schema: &serde_json::Value,
    ) -> Option<String> {
        self.unsupported_construct.map(ToOwned::to_owned)
    }

    fn pricing(&self, model: &ModelInfoV1) -> ModelPricing {
        PermissiveRules.pricing(model)
    }
}

// ── Quota Enforcement error builders ────────────────────────────────────────

/// Canonical errors shaped like the ones Quota Enforcement produces
/// (`quota-enforcement/src/infra/canonical_mapping.rs`): where each reason
/// token rides depends on the category.
pub mod qe_errors {
    use toolkit::api::canonical_prelude::CanonicalError;
    use toolkit_canonical_errors::resource_error;

    #[resource_error(gts_id!("cf.core.qe.lease.v1~"))]
    struct LeaseResource;

    /// `FailedPrecondition` whose violation `type` is `token`
    /// (`LEASE_NOT_ACTIVE`, `METRIC_NOT_REGISTERED`, ...).
    #[must_use]
    pub fn precondition(token: &str) -> CanonicalError {
        LeaseResource::failed_precondition()
            .with_precondition_violation("subject", format!("{token} (test)"), token)
            .create()
    }

    /// `InvalidArgument` whose field violation reason is `token`
    /// (`TTL_OUT_OF_BOUNDS`).
    #[must_use]
    pub fn invalid_argument(token: &str) -> CanonicalError {
        LeaseResource::invalid_argument()
            .with_field_violation("ttl_secs", format!("{token} (test)"), token)
            .create()
    }

    /// `Aborted` with reason `token` (`LEASE_CONTENTION_TIMEOUT`,
    /// `IDEMPOTENCY_PAYLOAD_MISMATCH`).
    #[must_use]
    pub fn aborted(token: &str) -> CanonicalError {
        LeaseResource::aborted("aborted (test)")
            .with_reason(token)
            .create()
    }

    /// `ResourceExhausted` whose quota violation description is `token`
    /// (`LEASE_INFLIGHT_LIMIT_EXCEEDED`, `ENGINE_COST_EXCEEDED`).
    #[must_use]
    pub fn exhausted(token: &str) -> CanonicalError {
        LeaseResource::resource_exhausted("exhausted (test)")
            .with_quota_violation("(tenant, metric)", token)
            .create()
    }

    /// `PermissionDenied` as the PDP refusal lifts (`AUTHZ`).
    #[must_use]
    pub fn permission_denied() -> CanonicalError {
        LeaseResource::permission_denied()
            .with_reason("AUTHZ")
            .create()
    }

    /// `NotFound`.
    #[must_use]
    pub fn not_found() -> CanonicalError {
        LeaseResource::not_found("lease not found (test)")
            .with_resource("lease")
            .create()
    }

    /// `DeadlineExceeded`.
    #[must_use]
    pub fn deadline_exceeded() -> CanonicalError {
        LeaseResource::deadline_exceeded("deadline (test)").create()
    }

    /// `ServiceUnavailable`.
    #[must_use]
    pub fn unavailable() -> CanonicalError {
        CanonicalError::service_unavailable()
            .with_detail("DEPENDENCY_UNAVAILABLE: test")
            .create()
    }

    /// `Unauthenticated`: the service identity was refused.
    #[must_use]
    pub fn unauthenticated() -> CanonicalError {
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_EXPIRED")
            .create()
    }

    /// `Internal`.
    #[must_use]
    pub fn internal() -> CanonicalError {
        CanonicalError::internal("internal error").create()
    }
}

// ── Quota Enforcement client fake ───────────────────────────────────────────

/// One call the Quota Enforcement fake received; `subject` is the subject id
/// of the `SecurityContext` it was made under.
#[derive(Debug, Clone)]
pub enum QuotaCall {
    /// `acquire_lease`.
    Acquire {
        subject: Uuid,
        request: AcquireLeaseRequest,
    },
    /// `commit_lease`.
    Commit {
        subject: Uuid,
        request: CommitLeaseRequest,
        at: tokio::time::Instant,
    },
    /// `release_lease`.
    Release {
        subject: Uuid,
        request: ReleaseLeaseRequest,
        at: tokio::time::Instant,
    },
}

/// Subject id of the service identity the fakes hand out.
pub const SERVICE_SUBJECT: u128 = 77;

/// Scripted `QuotaEnforcementClientV1`: records calls and pops scripted
/// outcomes (default: `Acquired` with a lease valid for the requested TTL,
/// commit and release succeed).
pub struct FakeQuota {
    calls: Mutex<Vec<QuotaCall>>,
    acquire_script: Mutex<VecDeque<Result<AcquireLeaseOutcome, QuotaEnforcementError>>>,
    commit_script: Mutex<VecDeque<Result<(), QuotaEnforcementError>>>,
    release_script: Mutex<VecDeque<Result<(), QuotaEnforcementError>>>,
    tokens: AtomicUsize,
    park_acquire: AtomicBool,
    /// Notified when an `acquire_lease` call has started (only when parked).
    pub acquire_entered: Notify,
    /// Releases a parked `acquire_lease` call.
    pub acquire_release: Notify,
    log: Arc<CallLog>,
}

impl FakeQuota {
    /// Fake with default outcomes.
    #[must_use]
    pub fn new(log: Arc<CallLog>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            acquire_script: Mutex::new(VecDeque::new()),
            commit_script: Mutex::new(VecDeque::new()),
            release_script: Mutex::new(VecDeque::new()),
            tokens: AtomicUsize::new(0),
            park_acquire: AtomicBool::new(false),
            acquire_entered: Notify::new(),
            acquire_release: Notify::new(),
            log,
        })
    }

    /// Queue `acquire_lease` outcomes (consumed in order, then the default).
    pub fn script_acquire(
        &self,
        outcomes: impl IntoIterator<Item = Result<AcquireLeaseOutcome, QuotaEnforcementError>>,
    ) {
        self.acquire_script.lock().expect("script").extend(outcomes);
    }

    /// Queue `commit_lease` outcomes (consumed in order, then success).
    pub fn script_commit(
        &self,
        outcomes: impl IntoIterator<Item = Result<(), QuotaEnforcementError>>,
    ) {
        self.commit_script.lock().expect("script").extend(outcomes);
    }

    /// Queue `release_lease` outcomes (consumed in order, then success).
    pub fn script_release(
        &self,
        outcomes: impl IntoIterator<Item = Result<(), QuotaEnforcementError>>,
    ) {
        self.release_script.lock().expect("script").extend(outcomes);
    }

    /// Park every `acquire_lease` call until `acquire_release` is notified.
    pub fn park_acquire(&self) {
        self.park_acquire.store(true, Ordering::SeqCst);
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<QuotaCall> {
        self.calls.lock().expect("calls").clone()
    }

    /// The `acquire_lease` requests so far.
    #[must_use]
    pub fn acquires(&self) -> Vec<(Uuid, AcquireLeaseRequest)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                QuotaCall::Acquire { subject, request } => Some((subject, request)),
                _ => None,
            })
            .collect()
    }

    /// The `commit_lease` requests so far.
    #[must_use]
    pub fn commits(&self) -> Vec<(Uuid, CommitLeaseRequest)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                QuotaCall::Commit {
                    subject, request, ..
                } => Some((subject, request)),
                _ => None,
            })
            .collect()
    }

    /// The `release_lease` requests so far.
    #[must_use]
    pub fn releases(&self) -> Vec<(Uuid, ReleaseLeaseRequest)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                QuotaCall::Release {
                    subject, request, ..
                } => Some((subject, request)),
                _ => None,
            })
            .collect()
    }

    /// Settle calls (commit or release) so far.
    #[must_use]
    pub fn settle_calls(&self) -> usize {
        self.commits().len() + self.releases().len()
    }

    /// Times (tokio clock) of the commit and release calls, in order.
    #[must_use]
    pub fn settle_times(&self) -> Vec<tokio::time::Instant> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                QuotaCall::Commit { at, .. } | QuotaCall::Release { at, .. } => Some(at),
                QuotaCall::Acquire { .. } => None,
            })
            .collect()
    }

    /// Token the default `Acquired` outcome hands out for lease number `n`
    /// (0-based).
    #[must_use]
    pub fn token(n: usize) -> LeaseToken {
        LeaseToken::from(Uuid::from_u128(0x1000 + n as u128))
    }

    fn allowed() -> Decision {
        Decision::allowed_with_plan(BTreeMap::new())
    }

    fn unsupported<T>() -> Result<T, QuotaEnforcementError> {
        Err(qe_errors::internal())
    }
}

/// A `Denied` acquire outcome with `reason` and one violated quota.
pub fn denied(reason: &str) -> AcquireLeaseOutcome {
    AcquireLeaseOutcome::Denied {
        decision: Decision {
            result: DecisionResult::Denied {
                violated_quota_ids: vec![QuotaId::from(Uuid::from_u128(0xBAD))],
                reason: reason.to_owned(),
            },
            debit_plan: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
        },
    }
}

/// An `Acquired` outcome for `token` valid for `ttl_secs`.
pub fn acquired(token: LeaseToken, ttl_secs: i64) -> AcquireLeaseOutcome {
    AcquireLeaseOutcome::Acquired {
        token,
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(ttl_secs),
    }
}

#[async_trait]
impl QuotaEnforcementClientV1 for FakeQuota {
    async fn debit(
        &self,
        _ctx: &SecurityContext,
        _request: DebitRequest,
    ) -> Result<Decision, QuotaEnforcementError> {
        Self::unsupported()
    }

    async fn rollback(
        &self,
        _ctx: &SecurityContext,
        _request: RollbackRequest,
    ) -> Result<Decision, QuotaEnforcementError> {
        Self::unsupported()
    }

    async fn evaluate_preview(
        &self,
        _ctx: &SecurityContext,
        _request: PreviewRequest,
    ) -> Result<DecisionPreview, QuotaEnforcementError> {
        Self::unsupported()
    }

    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, QuotaEnforcementError> {
        self.log.push("quota_acquire");
        let ttl = request.ttl_secs.unwrap_or(0);
        self.calls.lock().expect("calls").push(QuotaCall::Acquire {
            subject: ctx.subject_id(),
            request,
        });
        if self.park_acquire.load(Ordering::SeqCst) {
            self.acquire_entered.notify_one();
            self.acquire_release.notified().await;
        }
        if let Some(outcome) = self.acquire_script.lock().expect("script").pop_front() {
            return outcome;
        }
        let n = self.tokens.fetch_add(1, Ordering::SeqCst);
        Ok(acquired(
            Self::token(n),
            i64::try_from(ttl).expect("ttl fits"),
        ))
    }

    async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        request: CommitLeaseRequest,
    ) -> Result<Decision, QuotaEnforcementError> {
        self.log.push("quota_commit");
        self.calls.lock().expect("calls").push(QuotaCall::Commit {
            subject: ctx.subject_id(),
            request,
            at: tokio::time::Instant::now(),
        });
        let scripted = self.commit_script.lock().expect("script").pop_front();
        scripted.unwrap_or(Ok(())).map(|()| Self::allowed())
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        request: ReleaseLeaseRequest,
    ) -> Result<Decision, QuotaEnforcementError> {
        self.log.push("quota_release");
        self.calls.lock().expect("calls").push(QuotaCall::Release {
            subject: ctx.subject_id(),
            request,
            at: tokio::time::Instant::now(),
        });
        let scripted = self.release_script.lock().expect("script").pop_front();
        scripted.unwrap_or(Ok(())).map(|()| Self::allowed())
    }

    async fn batch_debit(
        &self,
        _ctx: &SecurityContext,
        _request: BatchDebitRequest,
    ) -> Result<BatchDecision, QuotaEnforcementError> {
        Self::unsupported()
    }

    async fn snapshot(
        &self,
        _ctx: &SecurityContext,
        _request: SnapshotRequest,
    ) -> Result<PageResult<QuotaSnapshot>, QuotaEnforcementError> {
        Self::unsupported()
    }
}

// ── Service identity fake ───────────────────────────────────────────────────

/// Service identity fake: hands out a context whose subject is
/// [`SERVICE_SUBJECT`]; the first `fail_first` calls fail.
pub struct FakeIdentity {
    fail_first: AtomicUsize,
    calls: AtomicUsize,
    invalidations: AtomicUsize,
    invalidated: Mutex<Vec<u64>>,
}

impl FakeIdentity {
    /// Identity that always succeeds.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::failing_first(0)
    }

    /// Identity whose first `n` calls fail (credentials exchange failure).
    #[must_use]
    pub fn failing_first(n: usize) -> Arc<Self> {
        Arc::new(Self {
            fail_first: AtomicUsize::new(n),
            calls: AtomicUsize::new(0),
            invalidations: AtomicUsize::new(0),
            invalidated: Mutex::new(Vec::new()),
        })
    }

    /// Number of `invalidate` calls (the cached context was refused).
    #[must_use]
    pub fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::SeqCst)
    }

    /// Generations passed to `invalidate`, in order.
    #[must_use]
    pub fn invalidated_generations(&self) -> Vec<u64> {
        self.invalidated.lock().expect("invalidated").clone()
    }

    /// Number of `context` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The service `SecurityContext` of the fakes (distinct from [`ctx`]).
#[must_use]
pub fn service_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(SERVICE_SUBJECT))
        .subject_tenant_id(Uuid::from_u128(3))
        .subject_type("service")
        .build()
        .expect("service context")
}

#[async_trait]
impl ServiceIdentity for FakeIdentity {
    async fn context(&self) -> anyhow::Result<IdentityContext> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let failing = self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            anyhow::bail!("credentials exchange failed (test)");
        }
        // A new generation after every invalidation, like a re-exchange.
        Ok(IdentityContext {
            ctx: service_ctx(),
            generation: 1 + self.invalidations.load(Ordering::SeqCst) as u64,
        })
    }

    async fn invalidate(&self, generation: u64) {
        self.invalidated
            .lock()
            .expect("invalidated")
            .push(generation);
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

// ── Usage sink fake ─────────────────────────────────────────────────────────

/// Usage sink fake recording every report.
#[derive(Default)]
pub struct FakeUsageSink {
    reports: Mutex<Vec<UsageReport>>,
    log: Arc<CallLog>,
}

impl FakeUsageSink {
    /// Empty sink.
    #[must_use]
    pub fn new(log: Arc<CallLog>) -> Arc<Self> {
        Arc::new(Self {
            reports: Mutex::new(Vec::new()),
            log,
        })
    }

    /// Reports received so far.
    #[must_use]
    pub fn reports(&self) -> Vec<UsageReport> {
        self.reports.lock().expect("reports").clone()
    }
}

#[async_trait]
impl UsageSink for FakeUsageSink {
    async fn record(&self, report: UsageReport) {
        self.log.push("usage_sink");
        self.reports.lock().expect("reports").push(report);
    }
}

// ── Usage Collector fake ────────────────────────────────────────────────────

/// Usage Collector errors as the SDK classifies them.
pub mod uc_errors {
    use usage_collector_sdk::{UsageCollectorError, ValidationReason};
    use uuid::Uuid;

    /// Retryable batch-level (or per-record) unavailability.
    #[must_use]
    pub fn unavailable() -> UsageCollectorError {
        UsageCollectorError::ServiceUnavailable {
            retry_after_seconds: None,
            detail: "plugin unavailable (test)".to_owned(),
        }
    }

    /// Permanent per-record validation failure.
    #[must_use]
    pub fn invalid_argument() -> UsageCollectorError {
        UsageCollectorError::InvalidArgument {
            resource_type: usage_collector_sdk::USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "metadata".to_owned(),
            reason: ValidationReason::Validation,
            detail: "unknown metadata key (test)".to_owned(),
        }
    }

    /// Same dedup identity, different payload.
    #[must_use]
    pub fn conflict() -> UsageCollectorError {
        UsageCollectorError::idempotency_conflict("resp_x", Uuid::nil())
    }

    /// PDP denial.
    #[must_use]
    pub fn permission_denied() -> UsageCollectorError {
        UsageCollectorError::permission_denied("denied (test)")
    }

    /// Unclassified failure.
    #[must_use]
    pub fn internal() -> UsageCollectorError {
        UsageCollectorError::Internal {
            detail: "boom (test)".to_owned(),
        }
    }
}

/// Scripted answer of one `create_usage_records` call: the batch-level error,
/// or per-record outcomes aligned with the input (`Ok(())` = accepted).
pub type UcScript = Result<Vec<Result<(), UsageCollectorError>>, UsageCollectorError>;

/// One observed `create_usage_records` call.
#[derive(Debug, Clone)]
pub struct UcCall {
    /// Tokio time of the call (virtual under a paused clock).
    pub at: tokio::time::Instant,
    /// Subject of the calling context ([`SERVICE_SUBJECT`] for the service
    /// identity).
    pub subject: Uuid,
    /// The submitted records, in order.
    pub records: Vec<CreateUsageRecord>,
}

/// Usage Collector fake: records every `create_usage_records` call and
/// answers from a script (accepting everything once the script is used up).
/// Only `create_usage_records` is supported.
#[derive(Default)]
pub struct FakeUsageCollector {
    script: Mutex<VecDeque<UcScript>>,
    calls: Mutex<Vec<UcCall>>,
    parked: AtomicBool,
    panicking: AtomicBool,
}

impl FakeUsageCollector {
    /// Collector that accepts everything.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Queue the answers of the next calls, one per call.
    pub fn script(&self, answers: impl IntoIterator<Item = UcScript>) {
        self.script.lock().expect("script").extend(answers);
    }

    /// From now on every call hangs forever (an unreachable collector that
    /// never answers); the call is still recorded.
    pub fn park(&self) {
        self.parked.store(true, Ordering::SeqCst);
    }

    /// From now on every call panics (a crashing client); the call is still
    /// recorded.
    pub fn panic_on_call(&self) {
        self.panicking.store(true, Ordering::SeqCst);
    }

    /// The calls so far.
    #[must_use]
    pub fn calls(&self) -> Vec<UcCall> {
        self.calls.lock().expect("calls").clone()
    }

    /// Records of every call, flattened in call order.
    #[must_use]
    pub fn submitted(&self) -> Vec<CreateUsageRecord> {
        self.calls().into_iter().flat_map(|c| c.records).collect()
    }

    /// Idempotency keys (response ids) of every submitted record, in
    /// submission order (retries repeat).
    #[must_use]
    pub fn submitted_keys(&self) -> Vec<String> {
        self.submitted()
            .iter()
            .map(|r| r.idempotency_key.as_str().to_owned())
            .collect()
    }

    /// Response ids (idempotency keys) accepted so far: records of calls
    /// whose scripted outcome accepted them.
    #[must_use]
    pub fn delivered_keys(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .flat_map(|c| {
                c.records
                    .iter()
                    .map(|r| r.idempotency_key.as_str().to_owned())
            })
            .collect()
    }

    fn unsupported<T>() -> Result<T, UsageCollectorError> {
        Err(UsageCollectorError::Internal {
            detail: "not supported by the fake (test)".to_owned(),
        })
    }
}

#[async_trait]
impl UsageCollectorClientV1 for FakeUsageCollector {
    async fn create_usage_record(
        &self,
        _ctx: &SecurityContext,
        _record: CreateUsageRecord,
    ) -> Result<UsageRecord, UsageCollectorError> {
        Self::unsupported()
    }

    async fn create_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<CreateUsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError> {
        self.calls.lock().expect("calls").push(UcCall {
            at: tokio::time::Instant::now(),
            subject: ctx.subject_id(),
            records: records.clone(),
        });
        assert!(
            !self.panicking.load(Ordering::SeqCst),
            "usage collector fake panics (test)"
        );
        if self.parked.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let scripted = self.script.lock().expect("script").pop_front();
        match scripted {
            Some(Err(err)) => Err(err),
            Some(Ok(outcomes)) => Ok(records
                .into_iter()
                .zip(outcomes)
                .map(|(record, outcome)| outcome.map(|()| record.into_usage_record()))
                .collect()),
            None => Ok(records
                .into_iter()
                .map(|record| Ok(record.into_usage_record()))
                .collect()),
        }
    }

    async fn get_usage_record(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError> {
        Self::unsupported()
    }

    async fn query_aggregated_usage_records(
        &self,
        _ctx: &SecurityContext,
        _gts_id: UsageTypeGtsId,
        _query: &toolkit_odata::ODataQuery,
        _metadata_filter: &[MetadataFilter],
        _aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorError> {
        Self::unsupported()
    }

    async fn list_usage_records(
        &self,
        _ctx: &SecurityContext,
        _gts_id: UsageTypeGtsId,
        _query: &toolkit_odata::ODataQuery,
        _metadata_filter: &[MetadataFilter],
    ) -> Result<toolkit_odata::Page<UsageRecord>, UsageCollectorError> {
        Self::unsupported()
    }

    async fn deactivate_usage_record(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), UsageCollectorError> {
        Self::unsupported()
    }

    async fn create_usage_type(
        &self,
        _ctx: &SecurityContext,
        _usage_type: UsageType,
    ) -> Result<UsageType, UsageCollectorError> {
        Self::unsupported()
    }

    async fn get_usage_type(
        &self,
        _ctx: &SecurityContext,
        _gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorError> {
        Self::unsupported()
    }

    async fn list_usage_types(
        &self,
        _ctx: &SecurityContext,
        _query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<UsageType>, UsageCollectorError> {
        Self::unsupported()
    }

    async fn delete_usage_type(
        &self,
        _ctx: &SecurityContext,
        _gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorError> {
        Self::unsupported()
    }
}

/// Wait until `cond` holds, polling on the tokio clock (so it also works with
/// a paused clock, where the wait advances virtual time); panics after
/// `within`.
///
/// # Panics
///
/// When `cond` does not hold within `within`.
pub async fn eventually_within(within: std::time::Duration, what: &str, cond: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    // Coarser polling for long (virtual-clock) waits keeps them cheap.
    let step = (within / 1000).max(std::time::Duration::from_millis(5));
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(step).await;
    }
}

/// Wait (bounded) for `notify`.
///
/// # Panics
///
/// When it is not notified within one second.
pub async fn entered(notify: &Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(1), notify.notified())
        .await
        .expect("the expected call never started");
}

/// [`eventually_within`] with a one second bound (real-time tests).
pub async fn eventually(what: &str, cond: impl Fn() -> bool) {
    eventually_within(std::time::Duration::from_secs(1), what, cond).await;
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
    pub timeouts: TimeoutsConfig,
    pub quota_cfg: QuotaConfig,
    pub quota: Arc<FakeQuota>,
    pub identity: Arc<FakeIdentity>,
    pub sink: Arc<FakeUsageSink>,
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
            tool_schemas: FakeToolSchemas::new(log.clone()),
            rules: FakeRules::default(),
            limits: LimitsConfig::default(),
            stream: StreamConfig::default(),
            timeouts: TimeoutsConfig::default(),
            quota_cfg: QuotaConfig::default(),
            quota: FakeQuota::new(log.clone()),
            identity: FakeIdentity::new(),
            sink: FakeUsageSink::new(log),
        }
    }

    /// Serve a model of [`OPENAI_TYPE`] patched with `patch` (e.g.
    /// `|v| set_cost(v, Some(1000), Some(2000))`).
    pub fn set_model(&mut self, patch: impl FnOnce(&mut serde_json::Value)) {
        self.models = FakeModels::found(
            sample_model_info_patched(OPENAI_TYPE, patch),
            self.log.clone(),
        );
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

    /// Serve an embedding model (`supported_api = ["embedding"]`), so
    /// `create_embedding` passes the capability gate.
    pub fn enable_embeddings(&mut self) {
        self.enable_embeddings_with(|_| {});
    }

    /// [`Self::enable_embeddings`] with an extra patch on the model's wire JSON.
    pub fn enable_embeddings_with(&mut self, patch: impl FnOnce(&mut serde_json::Value)) {
        self.models = FakeModels::found(
            sample_model_info_patched(OPENAI_TYPE, |v| {
                v["supported_api"] = serde_json::json!(["embedding"]);
                patch(v);
            }),
            self.log.clone(),
        );
    }

    /// The Quota Guard over the current fakes.
    #[must_use]
    pub fn quota_guard(&self) -> QuotaGuard {
        self.quota_guard_with(self.sink.clone())
    }

    /// The Quota Guard over the current fakes, handing usage to `sink`.
    #[must_use]
    pub fn quota_guard_with(&self, sink: Arc<dyn UsageSink>) -> QuotaGuard {
        QuotaGuard::new(
            self.quota.clone(),
            self.identity.clone(),
            sink,
            &self.quota_cfg,
            &self.timeouts,
        )
        // Deterministic backoff; jitter has its own test.
        .with_backoff_jitter(0.0)
    }

    /// Build the service over the current fakes.
    #[must_use]
    pub fn service(&self) -> Arc<Service> {
        self.service_with_quota(self.quota_guard())
    }

    /// Build the service over the current fakes with `quota` as its Quota
    /// Guard.
    #[must_use]
    pub fn service_with_quota(&self, quota: QuotaGuard) -> Arc<Service> {
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
            quota,
            &self.limits,
            &self.stream,
            &self.timeouts,
        ))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
