//! In-process implementation of `file_parser_sdk::FileParserClientV1`.
//!
//! Other gears depend on `file-parser-sdk` and resolve this client from
//! `ClientHub` instead of going over HTTP to `POST /file-parser/v1/upload`.
//! The gear registers this client in `ClientHub` during `init()` (see
//! `crate::gear`), and callers never see file-parser's own domain/infra
//! types.

use std::sync::Arc;

use async_trait::async_trait;
use file_parser_sdk::{FileParserClientError, FileParserClientV1, ParseBytesRequest, ParsedText};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::markdown::MarkdownRenderer;
use crate::domain::service::FileParserService;

impl From<DomainError> for FileParserClientError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::UnsupportedFileType { extension }
            | DomainError::NoParserAvailable { extension } => {
                Self::UnsupportedFileType { extension }
            }
            DomainError::InvalidRequest { message } => Self::InvalidRequest { message },
            DomainError::ParseError { message } => Self::ParseFailed { message },
            DomainError::FileNotFound { path } => Self::Internal(format!("file not found: {path}")),
            DomainError::IoError { message } | DomainError::PathTraversalBlocked { message } => {
                Self::Internal(message)
            }
        }
    }
}

/// In-process implementation of [`FileParserClientV1`], registered in
/// `ClientHub` by `FileParserGear::init`.
#[domain_model]
pub struct FileParserLocalClient {
    svc: Arc<FileParserService>,
}

impl FileParserLocalClient {
    /// Wrap the gear's domain service for `ClientHub` registration.
    #[must_use]
    pub fn new(svc: Arc<FileParserService>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl FileParserClientV1 for FileParserLocalClient {
    async fn parse_bytes(
        &self,
        _ctx: &SecurityContext,
        req: ParseBytesRequest,
    ) -> Result<ParsedText, FileParserClientError> {
        let document = self
            .svc
            .parse_bytes(
                req.filename.as_deref(),
                req.content_type.as_deref(),
                req.bytes,
                req.detection,
            )
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "file-parser: parse_bytes failed for in-process client");
                FileParserClientError::from(e)
            })?;

        Ok(ParsedText {
            markdown: MarkdownRenderer::render(&document),
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::path::PathBuf;

    use bytes::Bytes;
    use file_parser_sdk::Detection;
    use uuid::Uuid;

    use super::*;
    use crate::domain::service::ServiceConfig;
    use crate::infra::parsers::PlainTextParser;

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .expect("valid SecurityContext")
    }

    fn client_with_plain_text_backend() -> FileParserLocalClient {
        let service = FileParserService::new(
            vec![Arc::new(PlainTextParser::new())],
            ServiceConfig {
                max_file_size_bytes: 1024,
                allowed_local_base_dir: PathBuf::from("/tmp"),
            },
        );
        FileParserLocalClient::new(Arc::new(service))
    }

    #[tokio::test]
    async fn parse_bytes_returns_markdown_for_supported_type() {
        let client = client_with_plain_text_backend();
        let req = ParseBytesRequest {
            filename: Some("notes.txt".to_owned()),
            content_type: None,
            bytes: Bytes::from_static(b"hello world"),
            detection: Detection::Auto,
        };

        let result = client.parse_bytes(&ctx(), req).await;

        let parsed = result.expect("supported plain-text file should parse");
        assert!(
            parsed.markdown.contains("hello world"),
            "markdown should contain the source text: {:?}",
            parsed.markdown
        );
    }

    #[tokio::test]
    async fn parse_bytes_maps_unsupported_extension_to_client_error() {
        let client = client_with_plain_text_backend();
        let req = ParseBytesRequest {
            filename: Some("archive.zip".to_owned()),
            content_type: None,
            bytes: Bytes::from_static(b"not really a zip"),
            detection: Detection::Auto,
        };

        let result = client.parse_bytes(&ctx(), req).await;

        let err = result.expect_err("unsupported extension should fail");
        assert!(
            matches!(err, FileParserClientError::UnsupportedFileType { .. }),
            "expected UnsupportedFileType, got: {err:?}"
        );
    }

    #[test]
    fn domain_errors_map_to_correct_client_error_variants() {
        let cases: Vec<(DomainError, &str)> = vec![
            (
                DomainError::unsupported_file_type("zip"),
                "UnsupportedFileType",
            ),
            (
                DomainError::no_parser_available("zip"),
                "UnsupportedFileType",
            ),
            (
                DomainError::invalid_request("bad request"),
                "InvalidRequest",
            ),
            (DomainError::parse_error("broken document"), "ParseFailed"),
            (DomainError::file_not_found("/tmp/missing"), "Internal"),
            (DomainError::io_error("disk error"), "Internal"),
            (DomainError::path_traversal_blocked("blocked"), "Internal"),
        ];

        for (domain_err, expected_variant) in cases {
            let client_err: FileParserClientError = domain_err.clone().into();
            let actual_variant = match client_err {
                FileParserClientError::UnsupportedFileType { .. } => "UnsupportedFileType",
                FileParserClientError::InvalidRequest { .. } => "InvalidRequest",
                FileParserClientError::ParseFailed { .. } => "ParseFailed",
                FileParserClientError::Internal(_) => "Internal",
            };
            assert_eq!(
                actual_variant, expected_variant,
                "DomainError {domain_err:?} should map to {expected_variant}, got {actual_variant}"
            );
        }
    }

    #[test]
    fn domain_error_messages_are_preserved_in_client_error() {
        let client_err: FileParserClientError =
            DomainError::invalid_request("file too large").into();
        assert!(
            client_err.to_string().contains("file too large"),
            "client error should preserve the original message: {client_err}"
        );

        let client_err: FileParserClientError = DomainError::io_error("disk full").into();
        assert!(
            client_err.to_string().contains("disk full"),
            "internal error should preserve the original message: {client_err}"
        );
    }
}
