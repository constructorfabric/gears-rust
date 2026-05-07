//! Error type for the test-only microchat module.

use file_storage_sdk::FileStorageError;

#[derive(Debug, thiserror::Error)]
pub enum MicrochatError {
    #[error("mime type not allowed: {0}")]
    MimeNotAllowed(String),

    #[error("invalid filename: {0}")]
    InvalidFilename(&'static str),

    #[error("quota exceeded: max {max} files per user")]
    QuotaExceeded { max: u32 },

    #[error("attachment not found")]
    NotFound,

    #[error("file storage: {0}")]
    FileStorage(#[from] FileStorageError),

    #[error("database: {0}")]
    Database(String),
}
