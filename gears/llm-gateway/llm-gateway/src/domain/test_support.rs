//! Shared fakes and builders for domain, infra and REST unit tests.
//!
//! Every helper is test-only, so `expect` on internal mutexes is fine.
#![allow(clippy::missing_panics_doc)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, EqPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use llm_gateway_sdk::models::core::{
    ReasoningConfig, ResponseStatus, TextFormat, ToolChoice, ToolChoiceMode, TruncationStrategy,
};
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayError,
    LlmGatewayProviderPluginClientV1, MediaInputMode, ProviderCallCtx, ProviderPluginCapabilities,
    ResponseEventStream, ResponseInput, ResponseResource,
};
use model_registry_sdk::{ModelInfoV1, ModelV1};
use secrecy::SecretString;
use tokio::sync::Notify;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::Service;
use super::ports::{CredentialBroker, ModelResolver, PluginDirectory};
use crate::config::LimitsConfig;

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
    let media = serde_json::json!({ "enabled": false, "supported_mime_types": [] });
    let value = serde_json::json!({
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
    log: Arc<CallLog>,
    /// Notified when a call has entered the plugin (only when `gated`).
    pub entered: Notify,
    /// Releases a parked call (only when `gated`).
    pub release: Notify,
    gated: bool,
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
            log,
            entered: Notify::new(),
            release: Notify::new(),
            gated,
        })
    }

    /// Number of `create_response` calls.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
        ProviderPluginCapabilities::new(MediaInputMode::UrlForward)
    }

    async fn create_response(
        &self,
        _ctx: &SecurityContext,
        call: &ProviderCallCtx,
        _body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugin");
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
        _call: &ProviderCallCtx,
        _body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        Err(LlmGatewayError::internal("test fake: no streaming"))
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
    async fn resolve(
        &self,
        provider_type: &str,
    ) -> Result<Arc<dyn LlmGatewayProviderPluginClientV1>, LlmGatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.push("plugins");
        self.requested
            .lock()
            .expect("requested")
            .push(provider_type.to_owned());
        if provider_type == self.provider_type {
            Ok(self.plugin.clone())
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
    pub limits: LimitsConfig,
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
            broker: FakeBroker::with(|| Ok(None), log),
            limits: LimitsConfig::default(),
        }
    }

    /// Replace the plugin (and the directory that serves it).
    pub fn set_plugin(&mut self, plugin: Arc<FakePlugin>) {
        self.plugins = FakePluginDirectory::new(OPENAI_TYPE, plugin.clone(), self.log.clone());
        self.plugin = plugin;
    }

    /// Build the service over the current fakes.
    #[must_use]
    pub fn service(&self) -> Arc<Service> {
        Arc::new(Service::new(
            PolicyEnforcer::new(self.pdp.clone()),
            self.models.clone(),
            self.plugins.clone(),
            self.broker.clone(),
            &self.limits,
        ))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
