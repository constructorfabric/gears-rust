use thiserror::Error;
use toolkit_macros::domain_model;

/// Domain-level errors for file parsing operations
#[domain_model]
#[derive(Error, Debug, Clone)]
pub enum DomainError {
    #[error("File not found: {path}")]
    FileNotFound { path: String },

    #[error("Unsupported file type: {extension}")]
    UnsupportedFileType { extension: String },

    #[error("No parser available for extension: {extension}")]
    NoParserAvailable { extension: String },

    #[error("Parse error: {message}")]
    ParseError { message: String },

    #[error("IO error: {message}")]
    IoError { message: String },

    #[error("Invalid request: {message}")]
    InvalidRequest { message: String },

    #[error("Path traversal blocked: {message}")]
    PathTraversalBlocked { message: String },
}

impl DomainError {
    pub fn file_not_found(path: impl Into<String>) -> Self {
        Self::FileNotFound { path: path.into() }
    }

    pub fn unsupported_file_type(extension: impl Into<String>) -> Self {
        Self::UnsupportedFileType {
            extension: extension.into(),
        }
    }

    pub fn no_parser_available(extension: impl Into<String>) -> Self {
        Self::NoParserAvailable {
            extension: extension.into(),
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::ParseError {
            message: message.into(),
        }
    }

    pub fn io_error(message: impl Into<String>) -> Self {
        Self::IoError {
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    pub fn path_traversal_blocked(message: impl Into<String>) -> Self {
        Self::PathTraversalBlocked {
            message: message.into(),
        }
    }
}
