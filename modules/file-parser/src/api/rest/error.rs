use modkit::api::canonical_prelude::CanonicalProblemMigrationExt;
use modkit_canonical_errors::{CanonicalError, Problem, resource_error};

use crate::domain::error::DomainError;

#[resource_error("gts.cf.file_parser.parser.file.v1~")]
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

// TODO(cpt-cf-errors-component-error-middleware): drop this impl once
// middleware injects trace_id/instance from request context. The
// `From<DomainError> for CanonicalError` impl above is the long-lived
// mapping; this wrapper exists only to keep handler signatures returning
// `Problem` until middleware lands.
impl From<DomainError> for Problem {
    fn from(err: DomainError) -> Self {
        Problem::from(CanonicalError::from(err)).with_temporary_request_context("/")
    }
}
