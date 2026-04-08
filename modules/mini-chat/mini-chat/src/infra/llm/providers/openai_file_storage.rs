//! `OpenAI` implementation of `FileStorageProvider`.
//!
//! Uses `/v1/{path}` URI pattern (no query params).
//! Delegates HTTP mechanics to `RagHttpClient`.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::domain::ports::{FileStorageError, FileStorageProvider, UploadFileParams};
use crate::infra::llm::provider_resolver::ProviderResolver;
use crate::infra::llm::providers::rag_http_client::RagHttpClient;

pub struct OpenAiFileStorage {
    client: Arc<RagHttpClient>,
    resolver: Arc<ProviderResolver>,
}

impl OpenAiFileStorage {
    #[must_use]
    pub fn new(client: Arc<RagHttpClient>, resolver: Arc<ProviderResolver>) -> Self {
        Self { client, resolver }
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
        Ok(format!("/{alias}/v1/{path}"))
    }
}

#[async_trait]
impl FileStorageProvider for OpenAiFileStorage {
    async fn upload_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: UploadFileParams,
    ) -> Result<(String, u64), FileStorageError> {
        let uri = self.resolve_uri(&ctx, provider_id, "files")?;
        self.client.multipart_upload(ctx, &uri, params).await
    }

    async fn delete_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        provider_file_id: &str,
    ) -> Result<(), FileStorageError> {
        let uri = self.resolve_uri(&ctx, provider_id, &format!("files/{provider_file_id}"))?;
        self.client.delete(ctx, &uri).await
    }
}
