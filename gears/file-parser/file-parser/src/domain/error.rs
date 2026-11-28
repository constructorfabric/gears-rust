use thiserror::Error;
use toolkit_macros::domain_model;

/// A parser limit a document can exceed. Its name is the `ParseLimits` field
/// that sets it, and is what callers see in the error.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseLimit {
    MaxZipEntries,
    MaxEntryBytes,
    MaxTotalUncompressedBytes,
    MaxCompressionRatio,
    MaxNestingDepth,
    MaxSheets,
    MaxHtmlNodes,
    MaxPdfPages,
    MaxPdfCharsPerPage,
    PdfTimeout,
    MaxTextBytes,
}

impl ParseLimit {
    /// The `ParseLimits` field name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxZipEntries => "max_zip_entries",
            Self::MaxEntryBytes => "max_entry_bytes",
            Self::MaxTotalUncompressedBytes => "max_total_uncompressed_bytes",
            Self::MaxCompressionRatio => "max_compression_ratio",
            Self::MaxNestingDepth => "max_nesting_depth",
            Self::MaxSheets => "max_sheets",
            Self::MaxHtmlNodes => "max_html_nodes",
            Self::MaxPdfPages => "max_pdf_pages",
            Self::MaxPdfCharsPerPage => "max_pdf_chars_per_page",
            Self::PdfTimeout => "pdf_timeout",
            Self::MaxTextBytes => "max_text_bytes",
        }
    }
}

impl std::fmt::Display for ParseLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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

    /// The document is over one of the parser limits. Reported to callers
    /// like [`DomainError::ParseError`]; kept apart so that parsers which skip
    /// a malformed optional part still stop on it.
    #[error("Parse error: document exceeds parser limit `{limit}`: {detail}")]
    LimitExceeded { limit: ParseLimit, detail: String },

    /// A shared engine is busy with other documents; the request can be
    /// retried.
    #[error("Busy: {message}")]
    Busy { message: String },

    #[error("IO error: {message}")]
    IoError { message: String },

    /// Our own failure, not the document's or the caller's: a parser that
    /// panicked, or a PDF engine that cannot be loaded or has stopped.
    #[error("Internal error: {message}")]
    Internal { message: String },

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

    pub fn limit_exceeded(limit: ParseLimit, detail: impl Into<String>) -> Self {
        Self::LimitExceeded {
            limit,
            detail: detail.into(),
        }
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::Busy {
            message: message.into(),
        }
    }

    pub fn io_error(message: impl Into<String>) -> Self {
        Self::IoError {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
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
