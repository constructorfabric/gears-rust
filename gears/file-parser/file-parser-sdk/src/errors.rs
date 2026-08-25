use thiserror::Error;

/// Transport-agnostic error type for [`crate::FileParserClientV1`].
#[derive(Debug, Clone, Error)]
pub enum FileParserClientError {
    /// The file type/extension isn't supported by any registered parser.
    #[error("unsupported file type: {extension}")]
    UnsupportedFileType {
        /// The unsupported extension.
        extension: String,
    },
    /// The request was rejected before parsing was attempted (e.g. the file
    /// exceeds the configured size limit, or no bytes were provided).
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Human-readable reason.
        message: String,
    },
    /// Parsing itself failed partway through.
    #[error("parse error: {message}")]
    ParseFailed {
        /// Human-readable reason.
        message: String,
    },
    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}
