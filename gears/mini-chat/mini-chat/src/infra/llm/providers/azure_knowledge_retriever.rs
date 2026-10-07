//! Azure `OpenAI` Knowledge Retriever (RAG via vector store search API).
//!
//! Implements `KnowledgeRetriever` using the Azure `OpenAI` vector store
//! search endpoint:
//! `POST /{alias}/openai/vector_stores/{id}/search?api-version={ver}`

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::domain::ports::FileStorageError;
use crate::domain::ports::knowledge_retriever::{
    KnowledgeRetriever, RetrievalError, RetrievalRequest, RetrievedChunk,
};
use crate::infra::llm::providers::rag_http_client::RagHttpClient;

/// Azure `OpenAI` vector store search response envelope.
#[derive(serde::Deserialize)]
struct SearchResponse {
    data: Vec<SearchResult>,
}

#[derive(serde::Deserialize)]
struct SearchResult {
    file_id: String,
    filename: String,
    score: f32,
    content: Vec<SearchContent>,
}

#[derive(serde::Deserialize)]
struct SearchContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

/// Implements knowledge retrieval via Azure `OpenAI` vector store search.
///
/// URI pattern:
/// `POST /{upstream_alias}/openai/vector_stores/{vector_store_id}/search?api-version={ver}`
pub struct AzureKnowledgeRetriever {
    client: Arc<RagHttpClient>,
}

impl AzureKnowledgeRetriever {
    #[must_use]
    pub fn new(client: Arc<RagHttpClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl KnowledgeRetriever for AzureKnowledgeRetriever {
    async fn retrieve(
        &self,
        ctx: SecurityContext,
        req: RetrievalRequest,
    ) -> Result<Vec<RetrievedChunk>, RetrievalError> {
        let uri = format!(
            "/{}/openai/vector_stores/{}/search?api-version={}",
            req.upstream_alias, req.vector_store_id, req.api_version
        );
        let body = serde_json::json!({
            "query": req.query,
            "max_num_results": req.top_k,
        });

        let response: SearchResponse =
            self.client
                .json_post(ctx, &uri, &body)
                .await
                .map_err(|e| match e {
                    FileStorageError::Rejected { message, .. } => RetrievalError::Rejected(message),
                    FileStorageError::Unavailable { message } => {
                        RetrievalError::Unavailable(message)
                    }
                    other => RetrievalError::Configuration(other.to_string()),
                })?;

        let chunks = response
            .data
            .into_iter()
            .map(|r| {
                let text = r
                    .content
                    .iter()
                    .filter(|c| c.content_type == "text")
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                RetrievedChunk {
                    source_uri: format!("kb://chat/{}/doc/{}", req.chat_id, r.file_id),
                    title: r.filename,
                    text,
                    score: r.score,
                }
            })
            .collect();

        Ok(chunks)
    }
}
