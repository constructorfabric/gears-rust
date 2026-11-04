//! Provider-plugin API trait for the LLM Gateway.
//!
//! A provider plugin isolates one LLM provider (`OpenAI`, Anthropic, Google, …)
//! or one provider API family (OpenAI-compatible) behind
//! [`LlmGatewayProviderPluginClientV1`] (`cpt-cf-llm-gateway-fr-provider-plugins-v1`).
//! It owns everything that differs per provider: translation (Open Responses ⇆
//! provider-native format, without third-party provider SDKs), the provider
//! call through the Outbound API Gateway, error and stop-reason normalization,
//! and the call cost in AI credits from the model's pricing. At startup it
//! registers its GTS plugin instance and publishes its provider settings
//! schema (including pricing) in Type Registry.
//!
//! Plugins register a scoped `ClientHub` entry under their GTS instance id
//! ([`crate::gts::LlmGatewayProviderPluginSpecV1`]); the core resolves the
//! plugin for a request from the resolved model's provider identity
//! (`ModelInfoV1.gts_type`) and delegates to this trait.
//!
//! Cross-cutting concerns — model resolution, capability checks, credential
//! retrieval, quota, usage reporting, timeouts — stay in the core. Async jobs,
//! batch, media generation, and realtime audio are out of scope for this
//! version.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::api::ResponseEventStream;
use crate::errors::LlmGatewayError;
use crate::models::core::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, ResponseResource,
};
use crate::models::plugin::{ProviderCallCtx, ProviderPluginCapabilities};

/// Provider-plugin API trait (Version 1).
///
/// Covers the P1 surfaces the core delegates per request: create-response
/// (sync and streaming) and embeddings. Every call receives a
/// [`ProviderCallCtx`] carrying the resolved model info as the GTS-typed
/// `ModelInfoV1` envelope and the optional provider API key, so the plugin
/// needs no Model Registry or `CredStore` access of its own.
///
/// Requests and responses are the same Open Responses–aligned SDK types the
/// core uses. Every response carries [`crate::models::core::Usage`] with the
/// cost in AI credits, generic token usage, and the provider's raw usage, plus
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
    /// with cost in AI credits.
    ///
    /// The plugin adapts a structured output schema to the provider's JSON
    /// Schema subset. Whether the schema can be expressed at all is checked by
    /// the Gateway service before quota admission and key retrieval
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
}
