//! In-process `LlmGatewayClientV1` registered in `ClientHub`.

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingRequest, EmbeddingResponse, LlmGatewayClientV1, LlmGatewayError,
    ResponseEventStream, ResponseResource,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::Service;

/// Local client wrapping the domain [`Service`].
#[domain_model]
pub struct LlmGatewayLocalClient {
    svc: Arc<Service>,
}

impl LlmGatewayLocalClient {
    /// Wrap `svc`.
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl LlmGatewayClientV1 for LlmGatewayLocalClient {
    async fn create_response(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseResource, LlmGatewayError> {
        self.svc.create_response(ctx, body).await
    }

    async fn create_response_stream(
        &self,
        ctx: &SecurityContext,
        body: CreateResponseBody,
    ) -> Result<ResponseEventStream, LlmGatewayError> {
        self.svc.create_response_stream(ctx, body).await
    }

    async fn create_embedding(
        &self,
        _ctx: &SecurityContext,
        _req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, LlmGatewayError> {
        Err(LlmGatewayError::capability_not_supported("embeddings"))
    }
}
