//! Azure `OpenAI` implementation of `VectorStoreProvider`.
//!
//! Uses `/openai/{path}?api-version={ver}` URI pattern.
//! Delegates HTTP mechanics to `RagHttpClient`.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::domain::ports::{AddFileToVectorStoreParams, FileStorageError, VectorStoreProvider};
use crate::infra::llm::provider_resolver::ProviderResolver;
use crate::infra::llm::providers::rag_http_client::RagHttpClient;

#[derive(Debug, Clone, serde::Deserialize)]
struct VectorStoreObject {
    id: String,
}

pub struct AzureVectorStore {
    client: Arc<RagHttpClient>,
    resolver: Arc<ProviderResolver>,
    api_version: String,
}

impl AzureVectorStore {
    #[must_use]
    pub fn new(
        client: Arc<RagHttpClient>,
        resolver: Arc<ProviderResolver>,
        api_version: String,
    ) -> Self {
        Self {
            client,
            resolver,
            api_version,
        }
    }

    fn resolve_uri(
        &self,
        ctx: &SecurityContext,
        provider_id: &str,
        path: &str,
    ) -> Result<String, FileStorageError> {
        let tenant_id = ctx.subject_tenant_id().to_string();
        let alias = self
            .resolver
            .upstream_alias_for(provider_id, Some(&tenant_id))
            .ok_or_else(|| FileStorageError::Configuration {
                message: format!("no OAGW alias for provider '{provider_id}'"),
            })?;
        Ok(format!(
            "/{alias}/openai/{path}?api-version={}",
            self.api_version
        ))
    }
}

#[async_trait]
impl VectorStoreProvider for AzureVectorStore {
    async fn create_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
    ) -> Result<String, FileStorageError> {
        let uri = self.resolve_uri(&ctx, provider_id, "vector_stores")?;
        let body = serde_json::json!({});
        self.client
            .json_post::<VectorStoreObject>(ctx, &uri, &body)
            .await
            .map(|vs| vs.id)
    }

    async fn add_file_to_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: AddFileToVectorStoreParams,
    ) -> Result<(), FileStorageError> {
        let uri = self.resolve_uri(
            &ctx,
            provider_id,
            &format!("vector_stores/{}/files", params.vector_store_id),
        )?;
        let body = serde_json::json!({
            "file_id": params.provider_file_id,
            "attributes": params.attributes,
        });
        self.client.json_post_no_response(ctx, &uri, &body).await
    }
}
