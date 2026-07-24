//! Provider-plugin API trait for the LLM Gateway.
//!
//! A provider plugin isolates one LLM provider (`OpenAI`, Anthropic, Google, …)
//! behind [`LlmGatewayProviderPluginClientV1`]. It owns two things the core
//! Gateway does not: translation (Open Responses ⇆ provider-native format) and
//! transport (the provider call, performed through the Outbound API Gateway).
//!
//! Plugins register a scoped `ClientHub` entry under their GTS instance id
//! ([`crate::gts::LlmGatewayProviderPluginSpecV1`]); the core resolves the
//! plugin for a request from the resolved model's provider identity
//! (`ModelInfoV1.gts_type`) and delegates to this trait.
//!
//! Cross-cutting concerns — model resolution, capability validation, hooks,
//! quota, usage, fallback, timeouts, `FileStorage` — stay in the core. Async
//! jobs, batch, and realtime audio are out of scope for this version.

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
/// `ModelInfoV1` envelope (which the plugin narrows to its typed view for
/// `provider_model_id`, `provider_settings`, and connection routing), so the
/// plugin needs no Model Registry access of its own.
/// Requests and responses are the
/// same Open Responses–aligned SDK types the core uses; errors are
/// [`LlmGatewayError`], which the core maps to the Open Responses error
/// contract.
#[async_trait]
pub trait LlmGatewayProviderPluginClientV1: Send + Sync {
    /// Report integration-level capabilities for this provider integration.
    /// Provider-wide and local — no external call.
    fn capabilities(&self) -> ProviderPluginCapabilities;

    /// Create a response (sync). Translates the request to the provider's
    /// native format, calls the provider via OAGW, and normalizes the result
    /// back into a [`ResponseResource`].
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
    /// Failures are in-band: the plugin surfaces a terminal error event and
    /// closes the stream. Pre-stream setup failures surface as the outer
    /// `Err`. Consumer-disconnect survival, hook assembly, and terminal
    /// `ResponseResource` assembly are handled by the core, not the plugin.
    ///
    /// See `cpt-cf-llm-gateway-seq-streaming-v1`.
    async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError>;

    /// Generate embeddings.
    ///
    /// See `cpt-cf-llm-gateway-seq-embeddings-v1`.
    async fn create_embedding(
        &self,
        ctx: &SecurityContext,
        call: &ProviderCallCtx,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError>;
}
