//! Domain types for upload idempotency.
//!
//! @cpt-cf-file-storage-fr-upload-idempotency

use toolkit_macros::domain_model;
use uuid::Uuid;

/// The stored response for an idempotency key lookup.
/// Returned to a retrying caller unchanged.
#[domain_model]
#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    pub file_id: Uuid,
    /// HTTP status code of the original response (e.g. 201).
    pub response_status: u16,
    /// JSON-serialized `UploadTicketDto` body.
    pub response_body: String,
    pub response_etag: String,
}
