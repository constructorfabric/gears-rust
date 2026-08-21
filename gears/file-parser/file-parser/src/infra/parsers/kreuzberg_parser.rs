use async_trait::async_trait;
use kreuzberg::{ExtractionConfig, extract_bytes, extract_file};
use std::path::Path;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::ir_convert::result_to_blocks;

/// Unified document parser backed by Kreuzberg.
///
/// Replaces the previously separate `HtmlParser`, `PdfParser`, `XlsxParser`, and
/// `PptxParser` with a single extraction pipeline that delegates all format-specific
/// logic to Kreuzberg.
///
/// # Supported formats
/// | Extension(s)           | MIME type                                                                    |
/// |------------------------|------------------------------------------------------------------------------|
/// | `pdf`                  | `application/pdf`                                                            |
/// | `html`, `htm`          | `text/html`                                                                  |
/// | `xlsx`                 | `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`          |
/// | `xls`                  | `application/vnd.ms-excel`                                                   |
/// | `xlsm`                 | `application/vnd.ms-excel.sheet.macroEnabled.12`                             |
/// | `xlsb`                 | `application/vnd.ms-excel.sheet.binary.macroEnabled.12`                      |
/// | `pptx`                 | `application/vnd.openxmlformats-officedocument.presentationml.presentation`  |
pub struct KreuzbergParser;

/// Extensions this backend can extract; standalone so `mime_for_ext` can
/// filter against it too.
const SUPPORTED_EXTENSIONS: &[&str] =
    &["pdf", "html", "htm", "xlsx", "xls", "xlsm", "xlsb", "pptx"];

impl KreuzbergParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn config() -> ExtractionConfig {
        ExtractionConfig {
            include_document_structure: true,
            ..Default::default()
        }
    }

    /// Map an extension to its canonical MIME, restricted to
    /// `supported_extensions()`: the shared table also carries extensions other
    /// backends own, which Kreuzberg would extract wrong.
    #[must_use]
    fn mime_for_ext(ext: &str) -> Option<&'static str> {
        if !SUPPORTED_EXTENSIONS
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext))
        {
            return None;
        }
        crate::domain::mime_table::mime_for_extension(ext)
    }
}

impl Default for KreuzbergParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for KreuzbergParser {
    fn id(&self) -> &'static str {
        "kreuzberg"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        SUPPORTED_EXTENSIONS
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        resolved_content_type: Option<&str>,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| DomainError::io_error("File path is not valid UTF-8"))?;

        // Prefer the resolved MIME type over re-deriving one from `path`.
        let mime = resolved_content_type.or_else(|| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            Self::mime_for_ext(ext)
        });

        let config = Self::config();
        let result = extract_file(path_str, mime, &config)
            .await
            .map_err(|e| DomainError::parse_error(format!("Kreuzberg extraction failed: {e}")))?;

        let blocks = result_to_blocks(&result);
        // Prefer the MIME type Kreuzberg detected over the hint we passed in;
        // leave content_type unset if neither Kreuzberg nor the hint has a value.
        let content_type = {
            let detected: &str = &result.mime_type;
            if detected.is_empty() {
                mime
            } else {
                Some(detected)
            }
        };

        let mut builder = DocumentBuilder::new(ParsedSource::LocalPath(path.display().to_string()))
            .blocks(blocks);
        if let Some(content_type) = content_type {
            builder = builder.content_type(content_type);
        }

        if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
            builder = builder.original_filename(filename);
            if let Some(title) = result.metadata.title {
                builder = builder.title(title);
            } else {
                builder = builder.title(filename);
            }
        }

        if let Some(lang) = result.metadata.language {
            builder = builder.language(lang);
        }

        Ok(builder.build())
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        bytes: bytes::Bytes,
    ) -> Result<crate::domain::ir::ParsedDocument, DomainError> {
        // Determine MIME type: prefer the explicit content_type, then fall back to
        // the file extension from the filename hint. Blank content_type and the
        // generic application/octet-stream are treated as absent so that
        // filename-based MIME inference activates the correct Kreuzberg extractor.
        let mime = content_type
            .map(str::trim)
            .filter(|ct| !ct.is_empty() && !ct.eq_ignore_ascii_case("application/octet-stream"))
            .or_else(|| {
                filename_hint
                    .and_then(|name| Path::new(name).extension())
                    .and_then(|ext| ext.to_str())
                    .and_then(|ext| Self::mime_for_ext(ext))
            })
            .ok_or_else(|| {
                DomainError::parse_error(
                    "Cannot determine MIME type: no content_type or recognized \
                     filename extension provided",
                )
            })?;

        let config = Self::config();
        let result = extract_bytes(&bytes, mime, &config)
            .await
            .map_err(|e| DomainError::parse_error(format!("Kreuzberg extraction failed: {e}")))?;

        let blocks = result_to_blocks(&result);
        let filename = filename_hint.unwrap_or("unknown");

        // Prefer the MIME type Kreuzberg detected; leave content_type unset if
        // neither Kreuzberg nor the hint has a non-empty value.
        let content_type = {
            let detected: &str = &result.mime_type;
            if detected.is_empty() {
                Some(mime)
            } else {
                Some(detected)
            }
        };

        let source = ParsedSource::Uploaded {
            original_name: filename.to_owned(),
        };

        let mut builder = DocumentBuilder::new(source)
            .blocks(blocks)
            .original_filename(filename);
        if let Some(content_type) = content_type {
            builder = builder.content_type(content_type);
        }

        if let Some(title) = result.metadata.title {
            builder = builder.title(title);
        } else {
            builder = builder.title(filename);
        }

        if let Some(lang) = result.metadata.language {
            builder = builder.language(lang);
        }

        Ok(builder.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_id() {
        let parser = KreuzbergParser::new();
        assert_eq!(parser.id(), "kreuzberg");
    }

    #[test]
    fn test_supported_extensions() {
        let parser = KreuzbergParser::new();
        let exts = parser.supported_extensions();
        assert!(exts.contains(&"pdf"));
        assert!(exts.contains(&"html"));
        assert!(exts.contains(&"xlsx"));
        assert!(exts.contains(&"pptx"));
    }

    #[test]
    fn test_mime_for_known_extensions() {
        assert_eq!(
            KreuzbergParser::mime_for_ext("pdf"),
            Some("application/pdf")
        );
        assert_eq!(KreuzbergParser::mime_for_ext("html"), Some("text/html"));
        assert_eq!(KreuzbergParser::mime_for_ext("htm"), Some("text/html"));
        assert_eq!(
            KreuzbergParser::mime_for_ext("xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
        assert_eq!(
            KreuzbergParser::mime_for_ext("pptx"),
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        );
    }

    #[test]
    fn test_mime_for_unknown_extension() {
        assert_eq!(KreuzbergParser::mime_for_ext("unknown"), None);
    }
}
