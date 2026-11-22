//! PDF documents, through libpdfium.
//!
//! [`engine`] reads each page's characters with their position, size,
//! direction and font style; [`layout`] turns them into lines and
//! [`structure`] the lines of the whole document into headings, list items,
//! rules and paragraphs.
//! Pages are separated by page breaks, empty pages included, so page numbers
//! can be recovered from the output. The title comes from the Info
//! dictionary unless it is blank or a producer's placeholder.

mod engine;
mod layout;
mod structure;
#[cfg(test)]
mod tests;

pub(crate) use engine::check_engine;

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, ParsedBlock, ParsedDocument, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::{ParseLimits, TextBudget};

/// Ceiling applied by [`PdfParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default; `Gear::init` passes the configured value.
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

const SUPPORTED_EXTENSIONS: &[&str] = &["pdf"];

const CONTENT_TYPE: &str = "application/pdf";

/// PDF parser.
pub struct PdfParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl PdfParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            limits: ParseLimits::default(),
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
        source: ParsedSource,
        filename: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let limits = self.limits.clone();
        let pdf = tokio::task::spawn_blocking(move || read_pdf(&bytes, &limits))
            .await
            .map_err(|e| DomainError::parse_error(format!("PDF task failed: {e}")))??;

        let mut builder = DocumentBuilder::new(source)
            .content_type(CONTENT_TYPE)
            .blocks(pdf.blocks);
        if let Some(title) = pdf.title.as_deref().or(filename) {
            builder = builder.title(title);
        }
        if let Some(filename) = filename {
            builder = builder.original_filename(filename);
        }
        Ok(builder.build())
    }
}

impl Default for PdfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for PdfParser {
    fn id(&self) -> &'static str {
        "pdf"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        SUPPORTED_EXTENSIONS
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        _resolved_content_type: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let bytes = read_bounded(path, self.max_bytes).await?;
        let filename = path.file_name().and_then(|s| s.to_str());
        let source = ParsedSource::LocalPath(path.display().to_string());
        self.parse(Bytes::from(bytes), source, filename).await
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        _content_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<ParsedDocument, DomainError> {
        let source = ParsedSource::Uploaded {
            original_name: filename_hint.unwrap_or("unknown").to_owned(),
        };
        self.parse(bytes, source, filename_hint).await
    }
}

struct Pdf {
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

fn read_pdf(bytes: &[u8], limits: &ParseLimits) -> Result<Pdf, DomainError> {
    let mut budget = TextBudget::new(limits);
    let mut pages = Vec::new();
    // Lines, not glyphs, are kept across pages: structure needs the whole
    // document (body size, running headers), and lines are charged as text.
    let info = engine::read_document(bytes, limits, &mut |page| {
        let lines = layout::page_lines(page.glyphs, &mut budget)?;
        pages.push(structure::Page {
            lines,
            area: page.area,
        });
        Ok(())
    })?;
    Ok(Pdf {
        title: info.title,
        blocks: structure::blocks(pages),
    })
}
