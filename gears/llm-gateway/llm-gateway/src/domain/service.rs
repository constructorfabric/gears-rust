//! Request pipeline (sync path): allocate id, authorize, take an in-flight
//! slot, resolve model, select plugin, resolve credentials, dispatch.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use llm_gateway_sdk::{
    CreateResponseBody, InputItem, LlmGatewayError, ProviderCallCtx, ProviderErrorCategory,
    ResponseInput, ResponseResource,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use super::authz::{actions, authorize};
use super::ports::{CredentialBroker, ModelResolver, PluginDirectory};
use crate::config::LimitsConfig;

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
    tracing::error!(response_id, code, error = %chain, "create_response failed");
}

fn log_warn(response_id: &str, code: &str, chain: &str) {
    tracing::warn!(response_id, code, error = %chain, "create_response failed");
}

fn log_info(response_id: &str, code: &str, chain: &str) {
    tracing::info!(response_id, code, error = %chain, "create_response failed");
}

fn has_item_reference(body: &CreateResponseBody) -> bool {
    matches!(
        &body.input,
        Some(ResponseInput::Items(items))
            if items.iter().any(|item| matches!(item, InputItem::ItemReference(_)))
    )
}

/// Domain service running the request pipeline.
#[domain_model]
pub struct Service {
    enforcer: PolicyEnforcer,
    models: Arc<dyn ModelResolver>,
    plugins: Arc<dyn PluginDirectory>,
    credentials: Arc<dyn CredentialBroker>,
    inflight_sync: Arc<Semaphore>,
}

impl Service {
    /// Build the service from its ports and the configured limits.
    #[must_use]
    pub fn new(
        enforcer: PolicyEnforcer,
        models: Arc<dyn ModelResolver>,
        plugins: Arc<dyn PluginDirectory>,
        credentials: Arc<dyn CredentialBroker>,
        limits: &LimitsConfig,
    ) -> Self {
        Self {
            enforcer,
            models,
            plugins,
            credentials,
            inflight_sync: Arc::new(Semaphore::new(limits.max_inflight_sync)),
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
        let span = |stage: &'static str| tracing::info_span!("llm_gateway.stage", stage, response_id = %response_id, model = %model);

        authorize(&self.enforcer, ctx, actions::CREATE_RESPONSE)
            .instrument(span("authorize"))
            .await?;

        // Held to the end of the function: released on every exit path.
        let _slot = {
            let _enter = span("acquire_slot").entered();
            Arc::clone(&self.inflight_sync)
                .try_acquire_owned()
                .map_err(|_| LlmGatewayError::GatewayUnavailable)?
        };

        {
            let _enter = span("capability_gate").entered();
            if has_item_reference(&body) {
                return Err(LlmGatewayError::capability_not_supported("item_reference"));
            }
        }

        let model_info = self
            .models
            .resolve(ctx, &model)
            .instrument(span("resolve_model"))
            .await?;

        let plugin = self
            .plugins
            .resolve(model_info.gts_type.as_ref())
            .instrument(span("select_plugin"))
            .await?;

        let api_key = self
            .credentials
            .resolve(ctx, &model_info)
            .instrument(span("resolve_credentials"))
            .await?;

        let mut call = ProviderCallCtx::new(model_info, response_id, cancel);
        if let Some(key) = api_key {
            call = call.with_api_key(key);
        }

        // One call, never retried: LLM calls are not idempotent.
        let mut resource = plugin
            .create_response(ctx, &call, body)
            .instrument(span("dispatch"))
            .await?;
        response_id.clone_into(&mut resource.id);
        Ok(resource)
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
