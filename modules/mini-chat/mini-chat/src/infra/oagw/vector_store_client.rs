use std::sync::Arc;

use modkit_security::SecurityContext;
use oagw_sdk::{Body, ServiceGatewayClientV1};
use tracing::debug;

use crate::domain::error::DomainError;

use super::validate_path_segment;

/// Client for OAGW Vector Store API.
pub struct OagwVectorStoreClient {
    gw: Arc<dyn ServiceGatewayClientV1>,
    alias: String,
}

impl OagwVectorStoreClient {
    pub fn new(gw: Arc<dyn ServiceGatewayClientV1>, alias: String) -> Self {
        Self { gw, alias }
    }

    /// Create a vector store at the provider. Returns the provider `vector_store_id`.
    pub async fn create_vector_store(
        &self,
        ctx: SecurityContext,
        name: &str,
    ) -> Result<String, DomainError> {
        let body = serde_json::json!({ "name": name });
        let body_bytes = serde_json::to_vec(&body).map_err(|e| DomainError::Internal {
            message: format!("json serialize failed: {e}"),
        })?;

        let uri = format!("/{}/v1/vector_stores", self.alias);
        let req = http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("content-type", "application/json")
            .body(Body::Bytes(body_bytes.into()))
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build request: {e}"),
            })?;

        let resp = self
            .gw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("OAGW create vector store failed: {e}"),
            })?;

        let (parts, body) = resp.into_parts();
        if !parts.status.is_success() {
            return Err(DomainError::Internal {
                message: format!(
                    "provider create vector store failed with status {}",
                    parts.status
                ),
            });
        }

        let body_bytes = body.into_bytes().await.map_err(|e| DomainError::Internal {
            message: format!("failed to read response: {e}"),
        })?;

        let json: serde_json::Value =
            serde_json::from_slice(&body_bytes).map_err(|e| DomainError::Internal {
                message: format!("failed to parse response: {e}"),
            })?;

        let vs_id = json["id"]
            .as_str()
            .ok_or_else(|| DomainError::Internal {
                message: "missing 'id' in vector store response".to_owned(),
            })?
            .to_owned();

        debug!(vector_store_id = %vs_id, "Vector store created at provider");
        Ok(vs_id)
    }

    /// Add a file to a vector store at the provider.
    pub async fn add_file_to_vector_store(
        &self,
        ctx: SecurityContext,
        vector_store_id: &str,
        file_id: &str,
        attachment_id: &str,
        uploaded_at: &str,
    ) -> Result<(), DomainError> {
        validate_path_segment(vector_store_id, "vector_store_id")?;

        let body = serde_json::json!({
            "file_id": file_id,
            "chunking_strategy": { "type": "auto" },
            "metadata": {
                "attachment_id": attachment_id,
                "uploaded_at": uploaded_at,
            }
        });
        let body_bytes = serde_json::to_vec(&body).map_err(|e| DomainError::Internal {
            message: format!("json serialize failed: {e}"),
        })?;

        let uri = format!("/{}/v1/vector_stores/{}/files", self.alias, vector_store_id);
        let req = http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("content-type", "application/json")
            .body(Body::Bytes(body_bytes.into()))
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build request: {e}"),
            })?;

        let resp = self
            .gw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("OAGW add file to vector store failed: {e}"),
            })?;

        let status = resp.into_parts().0.status;
        if !status.is_success() {
            return Err(DomainError::Internal {
                message: format!("provider add file to vector store failed with status {status}"),
            });
        }

        debug!(vector_store_id = %vector_store_id, file_id = %file_id, "File added to vector store");
        Ok(())
    }

    /// Delete a vector store at the provider (best-effort).
    pub async fn delete_vector_store(
        &self,
        ctx: SecurityContext,
        vector_store_id: &str,
    ) -> Result<(), DomainError> {
        validate_path_segment(vector_store_id, "vector_store_id")?;
        let uri = format!("/{}/v1/vector_stores/{}", self.alias, vector_store_id);
        let req = http::Request::builder()
            .method("DELETE")
            .uri(&uri)
            .body(Body::Empty)
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build request: {e}"),
            })?;

        let resp = self
            .gw
            .proxy_request(ctx, req)
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("OAGW delete vector store failed: {e}"),
            })?;

        let status = resp.into_parts().0.status;
        if status.is_success() || status == http::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(DomainError::Internal {
                message: format!(
                    "OAGW delete vector store failed for {vector_store_id}: HTTP {status}"
                ),
            })
        }
    }
}
