//! Request pipeline (sync path): allocate id, authorize, take an in-flight
//! slot, resolve model, select plugin, resolve tools, run the capability
//! checks, resolve credentials, dispatch, validate structured output, attach
//! `ignored_parameters`.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, InputItem, LlmGatewayError,
    ProviderCallCtx, ProviderErrorCategory, ProviderFailure, ResponseEventStream, ResponseInput,
    ResponseResource, TextFormatKind, Usage,
};
use model_registry_sdk::ModelInfoV1;
use rust_decimal::Decimal;
use secrecy::SecretString;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use super::authz::{actions, authorize};
use super::capability_checker::{CapabilityChecker, EffectiveEmbedding, EffectiveRequest};
use super::output_validator::OutputValidator;
use super::ports::{
    CredentialBroker, ModelResolver, PluginDirectory, ResolvedPlugin, ToolSchemaSource,
};
use super::provider_rules::ProviderRulesRegistry;
use super::quota_guard::{Operation, QuotaGuard, Quote, RequestMeta, RequestQuota};
use super::stream_supervisor::{StreamParams, initial_snapshot, supervise};
use super::tool_resolver::ToolResolver;
use crate::config::{LimitsConfig, StreamConfig, TimeoutsConfig};

/// A failed create-response call together with the response `id` that was
/// allocated for it, so REST Problems can carry the id.
#[domain_model]
#[derive(Debug)]
pub struct CreateResponseFailure {
    /// The response `id` allocated at the start of the pipeline.
    pub response_id: String,
    /// The failure.
    pub error: LlmGatewayError,
}

/// A failed create-embedding call together with the request `id` that was
/// allocated for it, so REST Problems can carry the id.
#[domain_model]
#[derive(Debug)]
pub struct CreateEmbeddingFailure {
    /// The embedding request `id` (`emb_<uuid v7>`) allocated at the start of
    /// the pipeline.
    pub request_id: String,
    /// The failure.
    pub error: LlmGatewayError,
}

/// Render `err` and its full `source` chain (`a: b: c`). Server-side only:
/// the wire error stays redacted.
///
/// Provider errors carry provider-supplied text, so they are logged with the
/// SDK-truncated message (`to_response_error`), escaped so control characters
/// cannot forge log lines, plus the provider identifiers only.
fn error_chain(err: &LlmGatewayError) -> String {
    if let LlmGatewayError::Provider { .. } = err {
        let wire = err.to_response_error();
        return format!(
            "{} (provider_code={:?}, provider_request_id={:?})",
            wire.message.escape_debug(),
            wire.provider_code,
            wire.provider_request_id,
        );
    }
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Whether `err` maps to a 5xx response (DESIGN §4.1): a provider category
/// not known to be caller-side counts as server-side.
fn is_server_side(err: &LlmGatewayError) -> bool {
    match err {
        LlmGatewayError::Internal { .. }
        | LlmGatewayError::ModelResolutionUnavailable
        | LlmGatewayError::CredentialsUnavailable
        | LlmGatewayError::QuotaCheckUnavailable
        | LlmGatewayError::GatewayUnavailable
        | LlmGatewayError::ProviderNotSupported { .. } => true,
        LlmGatewayError::Provider { category, .. } => !matches!(
            category,
            ProviderErrorCategory::ContextLengthExceeded
                | ProviderErrorCategory::ContentFiltered
                | ProviderErrorCategory::InvalidRequest
                | ProviderErrorCategory::RateLimited
                | ProviderErrorCategory::AuthenticationFailed
        ),
        _ => false,
    }
}

/// Log a failed create-response call: `error` for `internal`, `warn` for
/// other server-side (5xx) failures, `info` for caller-side (4xx) ones.
/// Carries identifiers and the error chain only, never request content or
/// provider keys.
fn log_failure(response_id: &str, err: &LlmGatewayError) {
    let code = err.code();
    let chain = error_chain(err);
    if matches!(err, LlmGatewayError::Internal { .. }) {
        log_error(response_id, code, &chain);
    } else if is_server_side(err) {
        log_warn(response_id, code, &chain);
    } else {
        log_info(response_id, code, &chain);
    }
}

fn log_error(response_id: &str, code: &str, chain: &str) {
    tracing::error!(response_id, code, error = %chain, "gateway request failed");
}

fn log_warn(response_id: &str, code: &str, chain: &str) {
    tracing::warn!(response_id, code, error = %chain, "gateway request failed");
}

fn log_info(response_id: &str, code: &str, chain: &str) {
    tracing::info!(response_id, code, error = %chain, "gateway request failed");
}

/// A dispatched plugin call outlived `timeouts.provider_sync`: cancel the
/// request, settle the lease with the input-only estimate (the provider may
/// have spent tokens we cannot see, same as an abort after dispatch) and
/// answer `provider_unavailable`. The message is fixed: no provider content.
fn provider_timed_out(
    response_id: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    quota: RequestQuota,
) -> LlmGatewayError {
    tracing::warn!(
        response_id,
        timeout_secs = timeout.as_secs(),
        "provider call exceeded timeouts.provider_sync; cancelled"
    );
    cancel.cancel();
    let estimate = quota.estimator().estimate(0, None);
    quota.settle(estimate);
    LlmGatewayError::provider(
        ProviderErrorCategory::ProviderUnavailable,
        "provider call timed out",
        ProviderFailure::default(),
    )
}

fn has_item_reference(body: &CreateResponseBody) -> bool {
    matches!(
        &body.input,
        Some(ResponseInput::Items(items))
            if items.iter().any(|item| matches!(item, InputItem::ItemReference(_)))
    )
}

/// Which in-flight pool and gates a request goes through.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sync,
    Stream,
}

/// Output of the shared pre-dispatch pipeline.
#[domain_model]
struct Prepared {
    plugin: ResolvedPlugin,
    call: ProviderCallCtx,
    body: CreateResponseBody,
    ignored_parameters: Vec<String>,
    output_schema: Option<serde_json::Value>,
    /// Held until the call (sync) or the stream (stream) is over.
    slot: OwnedSemaphorePermit,
    /// The quota state of the request: its lease, if any.
    quota: RequestQuota,
}

/// Domain service running the request pipeline.
#[domain_model]
pub struct Service {
    enforcer: PolicyEnforcer,
    models: Arc<dyn ModelResolver>,
    plugins: Arc<dyn PluginDirectory>,
    credentials: Arc<dyn CredentialBroker>,
    tool_resolver: ToolResolver,
    checker: CapabilityChecker,
    rules: Arc<ProviderRulesRegistry>,
    quota: QuotaGuard,
    validator: OutputValidator,
    max_tools: usize,
    inflight_sync: Arc<Semaphore>,
    inflight_streams: Arc<Semaphore>,
    channel_capacity: usize,
    /// Bound of one sync or embeddings plugin call (`timeouts.provider_sync`),
    /// the base of the quota lease TTL.
    provider_sync: Duration,
}

impl Service {
    /// Build the service from its ports and the configured limits.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // ports, the quota guard and the config sections
    pub fn new(
        enforcer: PolicyEnforcer,
        models: Arc<dyn ModelResolver>,
        plugins: Arc<dyn PluginDirectory>,
        credentials: Arc<dyn CredentialBroker>,
        tool_schemas: Arc<dyn ToolSchemaSource>,
        rules: Arc<ProviderRulesRegistry>,
        quota: QuotaGuard,
        limits: &LimitsConfig,
        stream: &StreamConfig,
        timeouts: &TimeoutsConfig,
    ) -> Self {
        Self {
            enforcer,
            models,
            plugins,
            credentials,
            tool_resolver: ToolResolver::new(tool_schemas, limits.max_schema_bytes),
            checker: CapabilityChecker::new(limits, Arc::clone(&rules)),
            rules,
            quota,
            validator: OutputValidator,
            max_tools: limits.max_tools,
            inflight_sync: Arc::new(Semaphore::new(limits.max_inflight_sync)),
            inflight_streams: Arc::new(Semaphore::new(limits.max_inflight_streams)),
            channel_capacity: stream.channel_capacity,
            provider_sync: timeouts.provider_sync,
        }
    }

    /// Create a response (sync) and return it, or the failure with the
    /// allocated response `id`.
    ///
    /// # Errors
    ///
    /// Any [`LlmGatewayError`] of the sync pipeline (DESIGN §4.1).
    pub async fn create_response_traced(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, Box<CreateResponseFailure>> {
        // The id is allocated first so every failure can carry it.
        let response_id = format!("resp_{}", Uuid::now_v7().simple());
        match self.run_sync(ctx, body, &response_id).await {
            Ok(resource) => Ok(resource),
            Err(error) => {
                log_failure(&response_id, &error);
                Err(Box::new(CreateResponseFailure { response_id, error }))
            }
        }
    }

    async fn run_sync(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
        response_id: &str,
    ) -> Result<ResponseResource, LlmGatewayError> {
        // Cancelled on every exit path, including a dropped future (consumer
        // disconnect): the plugin observes it through `ProviderCallCtx::cancel`.
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();

        let model = body.model.clone();
        // `_slot` is held to the end of the function: released on every exit path.
        let Prepared {
            plugin,
            call,
            body,
            ignored_parameters,
            output_schema,
            slot: _slot,
            mut quota,
        } = self
            .prepare(ctx, body, response_id, cancel, Mode::Sync)
            .await?;
        let estimator = quota.estimator();
        let span = |stage: &'static str| tracing::info_span!("llm_gateway.stage", stage, response_id = %response_id, model = %model);

        // One call, never retried: LLM calls are not idempotent.
        quota.mark_dispatched();
        let dispatched = tokio::time::timeout(
            self.provider_sync,
            plugin
                .client
                .create_response(ctx, &call, body)
                .instrument(span("dispatch")),
        )
        .await;
        let mut resource = match dispatched {
            Ok(Ok(resource)) => resource,
            Err(_elapsed) => {
                return Err(provider_timed_out(
                    response_id,
                    self.provider_sync,
                    &call.cancel,
                    quota,
                ));
            }
            Ok(Err(err)) => {
                // A provider error carries no cost: nothing to commit.
                quota.settle(Usage::reported(0, 0, Decimal::ZERO));
                return Err(err);
            }
        };
        let usage = resource
            .usage
            .clone()
            .unwrap_or_else(|| estimator.estimate(0, None));

        if let Some(schema) = output_schema {
            let _enter = span("output_validation").entered();
            if let Err(err) = self.validator.validate(&schema, &resource) {
                // The tokens were spent: settle with the plugin's usage.
                quota.settle(usage);
                return Err(err);
            }
        }
        quota.settle(usage);

        let mut ignored = std::mem::take(&mut resource.ignored_parameters);
        ignored.extend(ignored_parameters);
        ignored.sort();
        ignored.dedup();
        resource.ignored_parameters = ignored;
        response_id.clone_into(&mut resource.id);
        Ok(resource)
    }

    /// The pre-dispatch pipeline shared by the sync and stream paths:
    /// authorize, take an in-flight slot (of the `mode`'s pool), reject
    /// `item_reference`, resolve model, select plugin, resolve tools, gate
    /// streaming (stream mode), run the capability checks and resolve
    /// credentials.
    async fn prepare(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
        response_id: &str,
        cancel: CancellationToken,
        mode: Mode,
    ) -> Result<Prepared, LlmGatewayError> {
        let model = body.model.clone();
        let span = |stage: &'static str| tracing::info_span!("llm_gateway.stage", stage, response_id = %response_id, model = %model);

        let slot = self
            .admit(ctx, actions::CREATE_RESPONSE, response_id, &model, mode)
            .await?;

        {
            let _enter = span("capability_gate").entered();
            if has_item_reference(&body) {
                return Err(LlmGatewayError::capability_not_supported("item_reference"));
            }
        }

        let (model_info, plugin) = self.resolve_target(ctx, response_id, &model).await?;

        let mut body = body;
        if let Some(tools) = body.tools.take() {
            // Cheap guard first: no Type Registry call for an oversized list.
            if tools.len() > self.max_tools {
                return Err(LlmGatewayError::validation_param("too many tools", "tools"));
            }
            body.tools = Some(
                self.tool_resolver
                    .resolve(ctx, tools)
                    .instrument(span("tool_resolution"))
                    .await?,
            );
        }

        if mode == Mode::Stream {
            let _enter = span("streaming_gate").entered();
            let model_streams =
                model_info.capabilities.streaming && !model_info.disabled_capabilities.streaming;
            if !model_streams || !plugin.capabilities().streaming_transport {
                return Err(LlmGatewayError::capability_not_supported("streaming"));
            }
        }

        let effective = {
            let _enter = span("capability_check").entered();
            self.checker.check(&model_info, &plugin, body)?
        };
        let EffectiveRequest {
            body,
            ignored_parameters,
        } = effective;
        let output_schema = match body.text.as_ref().map(|text| &text.format) {
            Some(TextFormatKind::JsonSchema {
                schema: Some(schema),
                ..
            }) => Some(schema.clone()),
            _ => None,
        };

        let op = match mode {
            Mode::Sync => Operation::Response,
            Mode::Stream => Operation::Stream,
        };
        let rules = self.rules.rules_for(model_info.gts_type.as_ref());
        let pricing = QuotaGuard::check_pricing(rules.as_ref(), &model_info, op)?;
        let reservation = self.quota.response_quote(pricing, &model_info, &body);
        let (quota, call) = self
            .admit_and_call(
                ctx,
                response_id,
                &model,
                model_info,
                op,
                &reservation,
                cancel,
            )
            .await?;
        Ok(Prepared {
            plugin,
            call,
            body,
            ignored_parameters,
            output_schema,
            slot,
            quota,
        })
    }

    /// Shared first steps: authorize `action`, then take an in-flight slot of
    /// the `mode`'s pool.
    async fn admit(
        &self,
        ctx: &SecurityContext,
        action: &str,
        request_id: &str,
        model: &str,
        mode: Mode,
    ) -> Result<OwnedSemaphorePermit, LlmGatewayError> {
        let span = |stage: &'static str| tracing::info_span!("llm_gateway.stage", stage, response_id = %request_id, model = %model);
        authorize(&self.enforcer, ctx, action)
            .instrument(span("authorize"))
            .await?;

        let _enter = span("acquire_slot").entered();
        let pool = match mode {
            Mode::Sync => &self.inflight_sync,
            Mode::Stream => &self.inflight_streams,
        };
        Arc::clone(pool)
            .try_acquire_owned()
            .map_err(|_| LlmGatewayError::GatewayUnavailable)
    }

    /// Shared model resolution and plugin selection.
    async fn resolve_target(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
        model: &str,
    ) -> Result<(ModelInfoV1, ResolvedPlugin), LlmGatewayError> {
        let span = |stage: &'static str| tracing::info_span!("llm_gateway.stage", stage, response_id = %request_id, model = %model);
        let model_info = self
            .models
            .resolve(ctx, model)
            .instrument(span("resolve_model"))
            .await?;
        let plugin = self
            .plugins
            .resolve(model_info.gts_type.as_ref())
            .instrument(span("select_plugin"))
            .await?;
        Ok((model_info, plugin))
    }

    /// Shared concurrent step: quota admission and credential resolution run
    /// together. A failed admission drops the fetched key unused; a failed
    /// credential resolution releases a lease acquired meanwhile.
    #[allow(clippy::too_many_arguments)] // one shared step of three pipelines
    async fn admit_and_call(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
        model: &str,
        model_info: ModelInfoV1,
        op: Operation,
        reservation: &Quote,
        cancel: CancellationToken,
    ) -> Result<(RequestQuota, ProviderCallCtx), LlmGatewayError> {
        let meta = RequestMeta {
            response_id: request_id.to_owned(),
            model: model.to_owned(),
            provider_type: model_info.gts_type.as_ref().to_owned(),
        };
        let (admission, key) = tokio::join!(
            self.quota
                .admit(ctx, meta, op, reservation)
                .instrument(tracing::info_span!(
                    "llm_gateway.stage",
                    stage = "quota_admission",
                    response_id = %request_id,
                    model = %model
                )),
            self.resolve_key(ctx, &model_info, request_id, model),
        );
        let quota = admission?;
        let api_key = match key {
            Ok(key) => key,
            Err(err) => {
                quota.release();
                return Err(err);
            }
        };
        let mut call = ProviderCallCtx::new(model_info, request_id, cancel);
        if let Some(key) = api_key {
            call = call.with_api_key(key);
        }
        Ok((quota, call))
    }

    /// Credential resolution.
    async fn resolve_key(
        &self,
        ctx: &SecurityContext,
        model_info: &ModelInfoV1,
        request_id: &str,
        model: &str,
    ) -> Result<Option<SecretString>, LlmGatewayError> {
        self.credentials
            .resolve(ctx, model_info)
            .instrument(tracing::info_span!(
                "llm_gateway.stage",
                stage = "resolve_credentials",
                response_id = %request_id,
                model = %model
            ))
            .await
    }

    /// Create a response (sync); SDK-shaped wrapper over
    /// [`Self::create_response_traced`].
    ///
    /// # Errors
    ///
    /// Any [`LlmGatewayError`] of the sync pipeline (DESIGN §4.1).
    pub async fn create_response(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        self.create_response_traced(ctx, body)
            .await
            .map_err(|failure| failure.error)
    }
}

impl Service {
    /// Create a response and stream it: the same pre-stream pipeline as the
    /// sync path (authorization, in-flight stream slot, checks, credentials),
    /// then the plugin stream is handed to the Stream Supervisor. Returns the
    /// failure with the allocated response `id` when anything fails before the
    /// stream starts; later failures are in-band `response.failed` events.
    ///
    /// # Errors
    ///
    /// Any pre-stream [`LlmGatewayError`] (DESIGN §4.1), including
    /// `capability_not_supported` when the model or plugin cannot stream.
    pub async fn create_response_stream_traced(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, Box<CreateResponseFailure>> {
        let response_id = format!("resp_{}", Uuid::now_v7().simple());
        match self.start_stream(ctx, body, &response_id).await {
            Ok(stream) => Ok(stream),
            Err(error) => {
                log_failure(&response_id, &error);
                Err(Box::new(CreateResponseFailure { response_id, error }))
            }
        }
    }

    async fn start_stream(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
        response_id: &str,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        // Cancelled on every pre-stream failure exit (and on a dropped
        // future); disarmed once the supervisor owns the token.
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let model = body.model.clone();

        let Prepared {
            plugin,
            call,
            body,
            ignored_parameters,
            output_schema,
            slot,
            mut quota,
        } = self
            .prepare(ctx, body, response_id, cancel.clone(), Mode::Stream)
            .await?;
        let snapshot = initial_snapshot(response_id, &body);
        let estimator = quota.estimator();

        // One call, never retried: LLM calls are not idempotent.
        quota.mark_dispatched();
        let started = plugin
            .client
            .create_response_stream(ctx, &call, body)
            .instrument(tracing::info_span!(
                "llm_gateway.stage",
                stage = "dispatch",
                response_id = %response_id,
                model = %model
            ))
            .await;
        let upstream = match started {
            Ok(upstream) => upstream,
            Err(err) => {
                quota.settle(Usage::reported(0, 0, Decimal::ZERO));
                return Err(err);
            }
        };

        guard.disarm();
        Ok(supervise(
            upstream,
            StreamParams {
                response_id: response_id.to_owned(),
                snapshot,
                channel_capacity: self.channel_capacity,
                cancel,
                output_schema,
                ignored_parameters,
                slot: Some(slot),
                estimator: Some(estimator),
                on_terminal: Some(Box::new(move |usage| quota.settle(usage))),
            },
        ))
    }

    /// SDK-shaped wrapper over [`Self::create_response_stream_traced`].
    ///
    /// # Errors
    ///
    /// Any pre-stream [`LlmGatewayError`] (DESIGN §4.1).
    pub async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        self.create_response_stream_traced(ctx, body)
            .await
            .map_err(|failure| failure.error)
    }
}

impl Service {
    /// Create embeddings and return them, or the failure with the allocated
    /// request `id`.
    ///
    /// # Errors
    ///
    /// Any [`LlmGatewayError`] of the embeddings pipeline (DESIGN §4.1).
    pub async fn create_embedding_traced(
        &self,
        ctx: &SecurityContext,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, Box<CreateEmbeddingFailure>> {
        let request_id = format!("emb_{}", Uuid::now_v7().simple());
        match self.run_embedding(ctx, req, &request_id).await {
            Ok(response) => Ok(response),
            Err(error) => {
                log_failure(&request_id, &error);
                Err(Box::new(CreateEmbeddingFailure { request_id, error }))
            }
        }
    }

    async fn run_embedding(
        &self,
        ctx: &SecurityContext,
        req: EmbeddingRequest,
        request_id: &str,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        // Cancelled on every exit path, including a dropped future.
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();

        let model = req.model.clone();
        let _slot = self
            .admit(
                ctx,
                actions::CREATE_EMBEDDING,
                request_id,
                &model,
                Mode::Sync,
            )
            .await?;
        let (model_info, plugin) = self.resolve_target(ctx, request_id, &model).await?;
        let rules = self.rules.rules_for(model_info.gts_type.as_ref());

        let EffectiveEmbedding {
            request,
            ignored_parameters,
        } = {
            let _enter = tracing::info_span!("llm_gateway.stage", stage = "capability_check", response_id = %request_id, model = %model).entered();
            self.checker.check_embedding(&model_info, req)?
        };

        let pricing = QuotaGuard::check_pricing(rules.as_ref(), &model_info, Operation::Embedding)?;
        let reservation = self.quota.embedding_quote(pricing, &request);
        let (mut quota, call) = self
            .admit_and_call(
                ctx,
                request_id,
                &model,
                model_info,
                Operation::Embedding,
                &reservation,
                cancel,
            )
            .await?;

        // One call, never retried: calls are not idempotent.
        quota.mark_dispatched();
        let dispatched = tokio::time::timeout(
            self.provider_sync,
            plugin
                .client
                .create_embedding(ctx, &call, request)
                .instrument(tracing::info_span!(
                    "llm_gateway.stage",
                    stage = "dispatch",
                    response_id = %request_id,
                    model = %model
                )),
        )
        .await;
        let mut response = match dispatched {
            Ok(Ok(response)) => response,
            Err(_elapsed) => {
                return Err(provider_timed_out(
                    request_id,
                    self.provider_sync,
                    &call.cancel,
                    quota,
                ));
            }
            Ok(Err(err)) => {
                quota.settle(Usage::reported(0, 0, Decimal::ZERO));
                return Err(err);
            }
        };
        quota.settle(response.usage.clone());

        let mut ignored = std::mem::take(&mut response.ignored_parameters);
        ignored.extend(ignored_parameters);
        ignored.sort();
        ignored.dedup();
        response.ignored_parameters = ignored;
        request_id.clone_into(&mut response.id);
        Ok(response)
    }

    /// SDK-shaped wrapper over [`Self::create_embedding_traced`].
    ///
    /// # Errors
    ///
    /// Any [`LlmGatewayError`] of the embeddings pipeline (DESIGN §4.1).
    pub async fn create_embedding(
        &self,
        ctx: &SecurityContext,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        self.create_embedding_traced(ctx, req)
            .await
            .map_err(|failure| failure.error)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_checks_tests.rs"]
mod service_checks_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_stream_tests.rs"]
mod service_stream_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_embedding_tests.rs"]
mod service_embedding_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_quota_tests.rs"]
mod service_quota_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_quota_stream_tests.rs"]
mod service_quota_stream_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_timeout_tests.rs"]
mod service_timeout_tests;
