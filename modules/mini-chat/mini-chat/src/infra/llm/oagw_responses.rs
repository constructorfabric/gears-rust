//! Response types for OAGW proxy calls to OpenAI-compatible APIs.
//!
//! Used by `AttachmentService` to parse responses from the Files API
//! and Vector Stores API.

use serde::Deserialize;

/// Response from `POST /v1/files` — the uploaded file object.
#[derive(Debug, Clone, Deserialize)]
pub struct FileObject {
    /// Provider-assigned file identifier (e.g., `"file-abc123"`).
    pub id: String,
}

/// Response from `POST /v1/vector_stores` — the created vector store.
#[derive(Debug, Clone, Deserialize)]
pub struct VectorStoreObject {
    /// Provider-assigned vector store identifier.
    pub id: String,
}

/// Response from `POST /v1/vector_stores/{id}/files` — the added file.
#[derive(Debug, Clone, Deserialize)]
pub struct VectorStoreFileObject {
    /// Provider-assigned file-in-store identifier.
    pub id: String,
    /// Status of the file addition (e.g., `"in_progress"`, `"completed"`).
    pub status: String,
}
