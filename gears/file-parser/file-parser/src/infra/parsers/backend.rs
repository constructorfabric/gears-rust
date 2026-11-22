//! What the document parsers share: one [`FileParserBackend`] built from a
//! [`DocumentFormat`], and the helpers around their blocking read.

use std::future::Future;
use std::marker::PhantomData;
use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, ParsedBlock, ParsedDocument, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::ParseLimits;

/// What one document format adds to a [`DocumentParser`]: its id, its
/// extensions and how to read a whole document.
pub trait DocumentFormat: Send + Sync + 'static {
    /// Parser id reported by `/info`.
    const ID: &'static str;
    const EXTENSIONS: &'static [&'static str];

    /// Reads `bytes`. `content_type` is the caller's hint; only HTML uses it
    /// (for the charset).
    fn read(
        bytes: Bytes,
        content_type: Option<String>,
        limits: ParseLimits,
    ) -> impl Future<Output = Result<ReadDocument, DomainError>> + Send;
}

/// A document as a format reads it.
pub struct ReadDocument {
    pub content_type: &'static str,
    pub title: Option<String>,
    pub language: Option<String>,
    pub blocks: Vec<ParsedBlock>,
}

impl ReadDocument {
    #[must_use]
    pub fn new(
        content_type: &'static str,
        title: Option<String>,
        blocks: Vec<ParsedBlock>,
    ) -> Self {
        Self {
            content_type,
            title,
            language: None,
            blocks,
        }
    }
}

/// The parser for format `F`: reads local files up to a size cap and
/// uploads, under [`ParseLimits`].
pub struct DocumentParser<F> {
    max_bytes: u64,
    limits: ParseLimits,
    // `fn() -> F`: the parser holds no `F`, so it is `Send + Sync` for any.
    format: PhantomData<fn() -> F>,
}

impl<F: DocumentFormat> DocumentParser<F> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            limits: ParseLimits::default(),
            format: PhantomData,
        }
    }

    /// Caps how much of a local file is read into memory.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Replaces the default [`ParseLimits`].
    #[must_use]
    pub fn with_limits(mut self, limits: ParseLimits) -> Self {
        self.limits = limits;
        self
    }

    async fn parse(
        &self,
        bytes: Bytes,
        content_type: Option<&str>,
        source: ParsedSource,
        filename: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let read = F::read(bytes, content_type.map(str::to_owned), self.limits.clone()).await?;
        let mut builder = document(
            source,
            read.content_type,
            read.blocks,
            read.title.as_deref(),
            filename,
        );
        if let Some(language) = read.language {
            builder = builder.language(language);
        }
        Ok(builder.build())
    }
}

impl<F: DocumentFormat> Default for DocumentParser<F> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<F: DocumentFormat> FileParserBackend for DocumentParser<F> {
    fn id(&self) -> &'static str {
        F::ID
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        F::EXTENSIONS
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        resolved_content_type: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let bytes = read_bounded(path, self.max_bytes).await?;
        let (source, filename) = local_source(path);
        self.parse(Bytes::from(bytes), resolved_content_type, source, filename)
            .await
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<ParsedDocument, DomainError> {
        let source = uploaded_source(filename_hint);
        self.parse(bytes, content_type, source, filename_hint).await
    }
}

/// One mebibyte.
pub const MIB: u64 = 1024 * 1024;

/// Default of `FileParserConfig::max_file_size_mb`.
pub const DEFAULT_MAX_MB: u64 = 100;

/// Ceiling on how much of a local file a parser reads into memory by default;
/// `Gear::init` passes the configured value.
pub const DEFAULT_MAX_BYTES: u64 = DEFAULT_MAX_MB * MIB;

/// Runs `read` on the blocking pool inside the caller's tracing span. A
/// panic in `read` is our bug, not the document's: it is logged and reported
/// as an internal error without the panic text.
// cancel-safe: dropping the future discards the result, but the read runs to
// the end on the blocking pool; `ParseLimits` bounds the work it can do, not
// the time it takes.
pub async fn run_blocking<T, F>(format: &'static str, read: F) -> Result<T, DomainError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DomainError> + Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(read))
        .await
        .map_err(|e| {
            tracing::error!(format, error = %e, "parser task failed");
            DomainError::internal(format!("{format} parser task failed"))
        })?
}

/// Source and file name of a local file.
fn local_source(path: &Path) -> (ParsedSource, Option<&str>) {
    (
        ParsedSource::LocalPath(path.display().to_string()),
        path.file_name().and_then(|s| s.to_str()),
    )
}

/// Source of an upload.
fn uploaded_source(filename_hint: Option<&str>) -> ParsedSource {
    ParsedSource::Uploaded {
        original_name: filename_hint.unwrap_or("unknown").to_owned(),
    }
}

/// A document builder with what every parser sets: content type, blocks,
/// the document's own title or else the file name, and the file name.
fn document(
    source: ParsedSource,
    content_type: &str,
    blocks: Vec<ParsedBlock>,
    title: Option<&str>,
    filename: Option<&str>,
) -> DocumentBuilder {
    let mut builder = DocumentBuilder::new(source)
        .content_type(content_type)
        .blocks(blocks);
    if let Some(title) = title.or(filename) {
        builder = builder.title(title);
    }
    if let Some(filename) = filename {
        builder = builder.original_filename(filename);
    }
    builder
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_parser_is_an_internal_error_without_the_panic_text() {
        let err = run_blocking("TEST", || -> Result<(), DomainError> {
            panic!("secret detail");
        })
        .await
        .expect_err("the task panicked");
        assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
        assert!(!err.to_string().contains("secret detail"), "{err}");
    }
}
