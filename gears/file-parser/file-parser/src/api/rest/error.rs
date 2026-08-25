use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.file_parser.parser.file.v1~"))]
pub struct FileParserError;

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::FileNotFound { path } => FileParserError::not_found("File not found")
                .with_resource(path)
                .create(),

            DomainError::UnsupportedFileType { extension } => FileParserError::invalid_argument()
                .with_field_violation(
                    "content_type",
                    format!("Unsupported file type: {extension}"),
                    "UNSUPPORTED_CONTENT_TYPE",
                )
                .create(),

            DomainError::NoParserAvailable { extension } => FileParserError::invalid_argument()
                .with_field_violation(
                    "content_type",
                    format!("No parser available for extension: {extension}"),
                    "UNSUPPORTED_CONTENT_TYPE",
                )
                .create(),

            DomainError::ParseError { message } => FileParserError::invalid_argument()
                .with_field_violation("body", message, "PARSE_ERROR")
                .create(),

            DomainError::IoError { message } => {
                tracing::error!(error = %message, "file-parser I/O error");
                CanonicalError::internal(message).create()
            }

            DomainError::InvalidRequest { message } => FileParserError::invalid_argument()
                .with_constraint(message)
                .create(),

            DomainError::PathTraversalBlocked { message } => {
                tracing::warn!(error = %message, "path traversal blocked");
                FileParserError::permission_denied()
                    .with_reason("PATH_TRAVERSAL_BLOCKED")
                    .create()
            }
        }
    }
}
