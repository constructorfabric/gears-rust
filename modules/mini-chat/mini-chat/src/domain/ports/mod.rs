//! Domain-level port traits for file storage, vector store operations,
//! and observability (metrics).
//!
//! These traits decouple domain services from provider-specific HTTP
//! details (URI paths, multipart encoding, response DTOs) and from
//! concrete telemetry implementations. Infrastructure implementations
//! live in `infra::llm::providers` and `infra::metrics`.

use std::collections::HashMap;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::Stream;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;

use super::error::DomainError;

/// Async byte stream used for streaming file uploads through the domain layer.
///
/// Identical shape to `oagw_sdk::BodyStream` — conversion at the infra
/// boundary is zero-cost. Defined here to keep the domain layer free of
/// HTTP / infra SDK dependencies (enforced by dylint).
pub type FileStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>> + Send>>;

pub(crate) mod metric_labels;
pub(crate) mod metrics;

pub(crate) use metrics::MiniChatMetricsPort;

// ── Error type ──────────────────────────────────────────────────────────

/// Errors from file storage / vector store provider operations.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum FileStorageError {
    /// Provider explicitly rejected the request (4xx).
    #[error("provider rejected request: {message}")]
    Rejected { code: String, message: String },

    /// Provider unavailable or transient failure (5xx, timeout).
    #[error("provider unavailable: {message}")]
    Unavailable { message: String },

    /// Configuration error (missing upstream alias, bad credentials).
    #[error("configuration error: {message}")]
    Configuration { message: String },

    /// Failed to parse the provider response.
    #[error("invalid provider response: {message}")]
    InvalidResponse { message: String },
}

impl From<FileStorageError> for DomainError {
    fn from(e: FileStorageError) -> Self {
        match e {
            FileStorageError::Rejected { code, message } => DomainError::ProviderError {
                code,
                sanitized_message: message,
            },
            FileStorageError::Unavailable { message } => DomainError::ProviderError {
                code: "provider_unavailable".to_owned(),
                sanitized_message: message,
            },
            FileStorageError::Configuration { message } => DomainError::ProviderError {
                code: "configuration_error".to_owned(),
                sanitized_message: message,
            },
            FileStorageError::InvalidResponse { message } => DomainError::ProviderError {
                code: "response_parse_error".to_owned(),
                sanitized_message: message,
            },
        }
    }
}

// ── Param structs ───────────────────────────────────────────────────────

/// Parameters for uploading a file to a provider.
///
/// `file_stream` is an async byte stream — the provider implementation
/// forwards chunks to OAGW without buffering the entire file.
#[domain_model]
pub struct UploadFileParams {
    pub filename: String,
    pub content_type: String,
    pub file_stream: FileStream,
    pub purpose: String,
}

/// Parameters for adding a file to a vector store.
#[domain_model]
pub struct AddFileToVectorStoreParams {
    pub vector_store_id: String,
    pub provider_file_id: String,
    pub attributes: HashMap<String, String>,
}

// ── Traits ──────────────────────────────────────────────────────────────

/// Port for file upload/delete operations against a storage provider.
#[async_trait]
pub trait FileStorageProvider: Send + Sync {
    /// Upload a file and return `(provider_file_id, bytes_uploaded)`.
    async fn upload_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: UploadFileParams,
    ) -> Result<(String, u64), FileStorageError>;

    /// Delete a file from the provider. Best-effort — errors are logged, not fatal.
    async fn delete_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        provider_file_id: &str,
    ) -> Result<(), FileStorageError>;
}

/// Port for vector store operations against a storage provider.
#[async_trait]
pub trait VectorStoreProvider: Send + Sync {
    /// Create a new vector store and return its provider-assigned ID.
    async fn create_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
    ) -> Result<String, FileStorageError>;

    /// Add a file to an existing vector store.
    async fn add_file_to_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: AddFileToVectorStoreParams,
    ) -> Result<(), FileStorageError>;
}
