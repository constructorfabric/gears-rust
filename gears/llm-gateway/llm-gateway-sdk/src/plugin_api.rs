//! Provider-plugin API trait for the LLM Gateway.
//!
//! A provider plugin isolates one LLM provider (`OpenAI`, Anthropic, Google, …)
//! or one provider API family (OpenAI-compatible) behind
//! [`LlmGatewayProviderPluginClientV1`] (`cpt-cf-llm-gateway-fr-provider-plugins-v1`).
//! It owns everything that differs per provider: translation (Open Responses ⇆
//! provider-native format, without third-party provider SDKs), the provider
//! call through the Outbound API Gateway, error and stop-reason normalization,
//! and the normalized usage counts the core prices the call from. At startup it
//! registers its GTS plugin instance and publishes its provider type, derived
//! from `gts.cf.genai.model.provider.v1~`, whose schema constrains the model's
//! `provider_settings`, in Types Registry.
//!
//! Plugins register a scoped `ClientHub` entry under their GTS instance id
//! ([`crate::gts::LlmGatewayProviderPluginSpecV1`]); the core resolves the
//! plugin for a request from the resolved model's provider identity
//! (`ModelInfoV1.gts_type`) and delegates to this trait.
//!
//! The plugin sends every provider call through OAGW to the model's
//! destination (`base_url`, or the provider's default URL) and selects the
//! model's credential by `secret_ref`; OAGW reads the secret from `CredStore`
//! and sets it on the outbound request. Neither the plugin nor the core reads
//! provider secrets.
//!
//! Cross-cutting concerns — model resolution, capability checks, cost
//! computation from the model's `cost` rates, quota, usage reporting,
//! timeouts — stay in the core. Async jobs,
//! batch, media generation, and realtime audio are out of scope for this
//! version.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::api::ResponseEventStream;
use crate::errors::LlmGatewayError;
use crate::models::core::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, ResponseResource,
};
use crate::models::passthrough::{PassthroughRequest, PassthroughResponse};
use crate::models::plugin::{ProviderCallCtx, ProviderPluginCapabilities};

/// Provider-plugin API trait (Version 1).
///
/// Covers the P1 surfaces the core delegates per request: create-response
/// (sync and streaming), embeddings, and, for plugins that implement the
/// pass-through API of their provider, pass-through calls. Every call receives a
/// [`ProviderCallCtx`] carrying the resolved `ModelInfoV1`, so the plugin
/// needs no Model Registry access of its own. It needs no `CredStore` access
/// either: the credential is named by `ModelInfoV1.secret_ref` and injected
/// by OAGW.
///
/// Requests and responses are the same Open Responses–aligned SDK types the
/// core uses. Every response carries [`crate::models::core::Usage`] with the
/// generic usage counts (tokens, cache reads and writes, web search calls) and
/// the provider's raw usage, with `cost` left at zero for the core to fill, plus
/// the provider's original stop reason and `provider_request_id` in
/// `provider_details`. Errors are [`LlmGatewayError`]; provider errors are
/// mapped to [`crate::errors::ProviderErrorCategory`] with the provider's
/// originals.
#[async_trait]
pub trait LlmGatewayProviderPluginClientV1: Send + Sync {
    /// Report integration-level capabilities for this provider integration.
    /// Provider-wide and local — no external call.
    fn capabilities(&self) -> ProviderPluginCapabilities;

    /// Create a response (sync). Translates the request to the provider's
    /// native format, calls the provider via OAGW, and normalizes the result
    /// back into a [`ResponseResource`]: status from the stop reason, usage
    /// as generic counts.
    ///
    /// The plugin adapts a structured output schema to the provider's JSON
    /// Schema subset. Whether the schema can be expressed at all is checked by
    /// the Gateway service before quota admission
    /// (`unsupported_schema`), not through this trait.
    ///
    /// See `cpt-cf-llm-gateway-seq-create-response-sync-v1`.
    async fn create_response(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError>;

    /// Create a response and stream it as [`crate::models::streaming::StreamingEvent`]s
    /// in `sequence_number` order.
    ///
    /// Failures after the stream started are in-band: the plugin emits a
    /// terminal `response.failed` event with the normalized error and closes
    /// the stream. Pre-stream failures surface as the outer `Err`. Dropping the
    /// returned stream must close the upstream provider connection. The core
    /// guarantees exactly one terminal event toward the consumer.
    ///
    /// See `cpt-cf-llm-gateway-seq-streaming-v1`.
    async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError>;

    /// Generate embeddings, converting vectors to the requested
    /// `encoding_format` when the provider cannot return it.
    ///
    /// See `cpt-cf-llm-gateway-seq-embeddings-v1`.
    async fn create_embedding(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError>;

    /// Forward a provider-native request on the plugin's pass-through route
    /// (`cpt-cf-llm-gateway-fr-pass-through-apis-v1`). Called only for a
    /// plugin whose [`ProviderPluginCapabilities::passthrough`] is set, with a
    /// path from its declared endpoints.
    ///
    /// In order, the plugin:
    /// 1. checks the body against its denylists of stateful fields and paid
    ///    provider tools, including the scan for `file_id`, `container`, and
    ///    `vector_store_ids` at any depth outside the consumer-defined
    ///    subtrees it skips, and on a match fails with
    ///    `capability_not_supported` carrying the field's JSON path as `param`
    ///    ([`LlmGatewayError::capability_not_supported_param`]) without
    ///    calling the provider;
    /// 2. replaces `model` with `provider_model_id`, forces no-store (`OpenAI`),
    ///    and for streaming requests usage in the stream when the provider
    ///    sends it only on request;
    /// 3. builds the provider headers itself (protocol headers from the
    ///    request, the credential named by `secret_ref` through OAGW) and
    ///    sends the native body through OAGW to the model's destination;
    /// 4. returns the native response or stream with `model` replaced back,
    ///    and the generic usage read from it (cost left at 0 for the core).
    ///
    /// Provider error responses are returned as
    /// [`PassthroughResponse::Json`] with the provider's status and body, not
    /// as `Err`. `Err` is for Gateway failures before a response
    /// (`capability_not_supported`, `credentials_unavailable`,
    /// `unapproved_destination`, transport failures), which the core renders
    /// in the route's native error format.
    ///
    /// The default implementation fails with `capability_not_supported`, for
    /// plugins that implement no pass-through API.
    async fn forward_passthrough(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        req: PassthroughRequest,
    ) -> Result<PassthroughResponse, LlmGatewayError> {
        let _ = (ctx, call);
        Err(LlmGatewayError::capability_not_supported(format!(
            "{}_passthrough",
            req.api.as_str()
        )))
    }
}
