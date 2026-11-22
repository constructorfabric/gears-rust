use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::{DomainError, ParseLimit};

/// Retry hint sent with a busy response.
const BUSY_RETRY_AFTER_SECONDS: u64 = 1;

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

            DomainError::LimitExceeded { limit, detail } => limit_exceeded(limit, &detail),

            DomainError::Busy { message } => busy(message),

            DomainError::IoError { message } => internal("io", message),

            DomainError::Internal { message } => internal("internal", message),

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

/// Reported like a parse error: the document is the problem.
fn limit_exceeded(limit: ParseLimit, detail: &str) -> CanonicalError {
    FileParserError::invalid_argument()
        .with_field_violation(
            "body",
            format!("document exceeds parser limit `{limit}`: {detail}"),
            "PARSE_ERROR",
        )
        .create()
}

/// Logged in full; callers get only the category (the description is not
/// serialised).
fn internal(kind: &'static str, message: String) -> CanonicalError {
    tracing::error!(kind, error = %message, "file-parser internal error");
    CanonicalError::internal(message).create()
}

/// A shared engine is taken; the client may retry shortly.
fn busy(message: String) -> CanonicalError {
    tracing::warn!(reason = %message, "file-parser busy");
    FileParserError::resource_exhausted(message.clone())
        .with_quota_violation("parser_engine", message)
        .with_quota_violation_retry_after_seconds(BUSY_RETRY_AFTER_SECONDS)
        .create()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use toolkit_canonical_errors::Problem;

    use super::*;

    fn problem(err: DomainError) -> Problem {
        Problem::from(CanonicalError::from(err))
    }

    #[test]
    fn a_limit_is_a_parse_error_naming_the_limit() {
        let p = problem(DomainError::limit_exceeded(
            ParseLimit::MaxTextBytes,
            "document text exceeds 10 bytes",
        ));
        assert_eq!(p.status, Some(400), "{p:?}");
        let violation = &p.context["field_violations"][0];
        assert_eq!(violation["field"], "body", "{p:?}");
        assert_eq!(violation["reason"], "PARSE_ERROR", "{p:?}");
        let description = violation["description"].as_str().unwrap_or_default();
        assert!(description.contains("`max_text_bytes`"), "{p:?}");
        assert!(description.contains("exceeds 10 bytes"), "{p:?}");
    }

    #[test]
    fn busy_is_retryable_resource_exhaustion() {
        let err = CanonicalError::from(DomainError::busy("the PDF engine is busy"));
        assert_eq!(err.retry_after_seconds(), Some(BUSY_RETRY_AFTER_SECONDS));
        let p = Problem::from(err);
        assert_eq!(p.status, Some(429), "{p:?}");
        assert_eq!(
            p.context["violations"][0]["subject"], "parser_engine",
            "{p:?}"
        );
    }

    #[test]
    fn internal_failures_do_not_reach_the_caller() {
        for err in [
            DomainError::internal("PDF engine: /tmp/x/libpdfium.so missing"),
            DomainError::io_error("Failed to read /srv/secret"),
        ] {
            let p = problem(err);
            assert_eq!(p.status, Some(500), "{p:?}");
            let shown = format!("{} {}", p.detail, p.context);
            assert!(
                !shown.contains("/tmp/x") && !shown.contains("/srv/secret"),
                "{shown}"
            );
        }
    }
}
