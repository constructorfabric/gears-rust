use bytes::Bytes;

/// Whether `file-parser` should run content-based type detection on a
/// request, or route purely off the caller-supplied `filename` /
/// `content_type` hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detection {
    /// Run content-based detection when a detector is registered — lets a
    /// confident detection correct a wrong or missing hint.
    #[default]
    Auto,
    /// Skip detection even when a detector is registered, for callers that
    /// already know the exact type they're passing. Detection costs latency
    /// and can misroute a type the caller is certain of.
    Skip,
}

/// Raw bytes to extract text from, plus the hints `file-parser` uses to pick
/// a backend (see `file_parser::domain::service::FileParserService::parse_bytes`).
#[derive(Debug, Clone)]
pub struct ParseBytesRequest {
    /// Original filename, if known — used to disambiguate the file type when
    /// `content_type` is absent or generic (e.g. `application/octet-stream`).
    pub filename: Option<String>,
    /// MIME type, if known.
    pub content_type: Option<String>,
    /// Raw file bytes.
    pub bytes: Bytes,
    /// Content-based type detection mode for this request.
    pub detection: Detection,
}

/// Extracted text for a parsed attachment, rendered as Markdown so it can be
/// scanned as plain text by consumers that only operate on `String` payloads,
/// or injected into an LLM prompt.
#[derive(Debug, Clone)]
pub struct ParsedText {
    /// Markdown rendering of the parsed document.
    pub markdown: String,
}
