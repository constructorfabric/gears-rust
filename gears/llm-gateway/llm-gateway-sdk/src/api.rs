// Created: 2026-07-10 by Constructor Tech
//! Public API trait for the LLM Gateway.
//!
//! [`LlmGatewayClientV1`] is registered in `ClientHub` by the module:
//! ```ignore
//! let gw = hub.get::<dyn LlmGatewayClientV1>()?;
//! let resp = gw.create_response(ctx, body).await?;
//! ```

use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use toolkit_security::SecurityContext;

use crate::errors::LlmGatewayError;
use crate::models::core::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, ResponseResource,
};
use crate::models::streaming::StreamingEvent;

/// Server-sent event stream produced by [`LlmGatewayClientV1::create_response_stream`].
///
/// Each item is a decoded [`StreamingEvent`]. Failures are in-band: the gateway,
/// as the stream producer, synthesizes a [`StreamingEvent::Failed`] /
/// [`StreamingEvent::Error`] event for any provider or transport error and then
/// closes the stream. Pre-stream setup failures surface as the outer `Result`
/// on the method instead. The `[DONE]` sentinel that closes an Open Responses
/// SSE stream is not surfaced as an item — it ends the stream.
pub type ResponseEventStream = Pin<Box<dyn Stream<Item = StreamingEvent> + Send>>;

/// Public API trait for the LLM Gateway (Version 1).
///
/// Covers the P1 Open Responses contracts: create-response (sync and
/// streaming) and embeddings. This trait is registered in `ClientHub` by the
/// llm-gateway module:
/// ```ignore
/// let gw = hub.get::<dyn LlmGatewayClientV1>()?;
/// let resp = gw.create_response(ctx, body).await?;
/// ```
///
/// All methods require `SecurityContext` for tenant scoping and authorization.
#[async_trait]
pub trait LlmGatewayClientV1: Send + Sync {
    /// Create a response (`POST /responses`), non-streaming.
    ///
    /// Returns the fully assembled [`ResponseResource`]. Background (async) jobs
    /// are handled by a separate API and are out of scope for this trait.
    ///
    /// See `cpt-cf-llm-gateway-seq-create-response-sync-v1`.
    async fn create_response(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError>;

    /// Create a response (`POST /responses`) and stream it as server-sent
    /// events.
    ///
    /// The returned [`ResponseEventStream`] yields [`StreamingEvent`]s in
    /// `sequence_number` order. Pre-stream failures (provider resolution,
    /// capability check, pre-call hook rejection) surface as the outer `Err`;
    /// once the first delta has been emitted the stream is committed to the
    /// selected provider (no fallback).
    ///
    /// See `cpt-cf-llm-gateway-seq-streaming-v1`.
    async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError>;

    /// Generate embeddings (`POST /embeddings`).
    ///
    /// See `cpt-cf-llm-gateway-seq-embeddings-v1`.
    async fn create_embedding(
        &self,
        ctx: &SecurityContext,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError>;
}
