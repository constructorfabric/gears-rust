use async_trait::async_trait;
use std::path::Path;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, Inline, ParsedBlock, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;

/// Plain text parser that handles text files
pub struct PlainTextParser {
    max_bytes: u64,
}

/// Ceiling applied by [`PlainTextParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default. `Gear::init` replaces it with the configured
/// value via [`PlainTextParser::with_max_bytes`].
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

impl PlainTextParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// Caps how much this parser reads from a local file into memory. The
    /// service's up-front `metadata()` check is not atomic against the file
    /// growing; this is what actually bounds the allocation.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for PlainTextParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for PlainTextParser {
    fn id(&self) -> &'static str {
        "plain_text"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        &["txt", "log", "md"]
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        _resolved_content_type: Option<&str>,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let content = read_bounded(path, self.max_bytes).await?;

        let text = String::from_utf8(content)
            .map_err(|e| DomainError::parse_error(format!("Failed to decode UTF-8: {e}")))?;

        let blocks = text_to_blocks(&text);

        let mut builder = DocumentBuilder::new(ParsedSource::LocalPath(path.display().to_string()))
            .content_type("text/plain")
            .blocks(blocks);

        if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
            builder = builder.title(filename).original_filename(filename);
        }

        Ok(builder.build())
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        _content_type: Option<&str>,
        bytes: bytes::Bytes,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|e| DomainError::parse_error(format!("Failed to decode UTF-8: {e}")))?;

        let blocks = text_to_blocks(&text);

        let source = ParsedSource::Uploaded {
            original_name: filename_hint.unwrap_or("unknown.txt").to_owned(),
        };

        let mut builder = DocumentBuilder::new(source)
            .content_type("text/plain")
            .blocks(blocks);

        if let Some(filename) = filename_hint {
            builder = builder.title(filename).original_filename(filename);
        }

        Ok(builder.build())
    }
}

/// Convert plain text into blocks by splitting on double newlines
fn text_to_blocks(text: &str) -> Vec<ParsedBlock> {
    text.split("\n\n")
        .filter(|para| !para.trim().is_empty())
        .map(|para| ParsedBlock::Paragraph {
            inlines: vec![Inline::plain(para.trim())],
        })
        .collect()
}
