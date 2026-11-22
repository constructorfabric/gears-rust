//! Word documents (`.docx`).
//!
//! The main part is found through the package relationships; styles and
//! numbering through its own. Paragraphs become headings (by outline level
//! or heading style), list items (by numbering), quotes and code blocks (by
//! style), or paragraphs with styled runs and external links. Tables keep
//! their grid (spans padded, merged cells empty); a table inside a cell is
//! read into the cell. Pictures with a description become images; text
//! boxes follow their paragraph. Tracked changes read as accepted; deleted
//! text, hidden text, field instructions, notes, comments, headers and
//! footers are left out.

mod body;
mod styles;
#[cfg(test)]
mod tests;

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, ParsedBlock, ParsedDocument, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::ParseLimits;
use super::ooxml::{self, Archive, Relationships};
use body::{Budgets, DocContext, read_body};
use styles::{Numbering, Styles, read_numbering, read_styles};

/// Ceiling applied by [`DocxParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default; `Gear::init` passes the configured value.
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

const SUPPORTED_EXTENSIONS: &[&str] = &["docx"];

const CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// DOCX parser.
pub struct DocxParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl DocxParser {
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
        let doc = tokio::task::spawn_blocking(move || read_docx(bytes, &limits))
            .await
            .map_err(|e| DomainError::parse_error(format!("DOCX task failed: {e}")))??;

        let mut builder = DocumentBuilder::new(source)
            .content_type(CONTENT_TYPE)
            .blocks(doc.blocks);
        if let Some(title) = doc.title.as_deref().or(filename) {
            builder = builder.title(title);
        }
        if let Some(filename) = filename {
            builder = builder.original_filename(filename);
        }
        Ok(builder.build())
    }
}

impl Default for DocxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for DocxParser {
    fn id(&self) -> &'static str {
        "docx"
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

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse DOCX: {detail}"))
}

fn tolerate<T: Default>(result: Result<T, DomainError>, what: &str) -> Result<T, DomainError> {
    ooxml::tolerate(result, "DOCX", what)
}

struct Docx {
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

fn read_docx(bytes: Bytes, limits: &ParseLimits) -> Result<Docx, DomainError> {
    let mut archive = Archive::open(bytes, "DOCX", limits)?;
    let root = archive.read_rels("")?;
    let main = root
        .of_type("officeDocument")
        .find(|r| !r.external && archive.contains(&r.target))
        .map(|r| r.target.clone())
        .or_else(|| {
            archive
                .contains("word/document.xml")
                .then(|| "word/document.xml".to_owned())
        })
        .ok_or_else(|| parse_error("no document part"))?;
    let core = root
        .of_type("core-properties")
        .find(|r| !r.external)
        .map(|r| r.target.clone())
        .or_else(|| {
            archive
                .contains("docProps/core.xml")
                .then(|| "docProps/core.xml".to_owned())
        });
    let title = match core {
        Some(part) => ooxml::core_title(&mut archive, &part)?,
        None => None,
    };

    let rels = tolerate(archive.read_rels(&main), "document relationships")?;
    let styles = match part_of(&rels, "styles", "word/styles.xml") {
        Some(part) => match tolerate(archive.read_xml(&part), "styles")? {
            Some(text) => tolerate(read_styles(&text, limits), "styles")?,
            None => Styles::default(),
        },
        None => Styles::default(),
    };
    let numbering = match part_of(&rels, "numbering", "word/numbering.xml") {
        Some(part) => match tolerate(archive.read_xml(&part), "numbering")? {
            Some(text) => tolerate(read_numbering(&text, limits), "numbering")?,
            None => Numbering::default(),
        },
        None => Numbering::default(),
    };

    let text = archive
        .read_xml(&main)?
        .ok_or_else(|| parse_error("no document part"))?;
    let ctx = DocContext {
        styles: &styles,
        numbering: &numbering,
        rels: &rels,
        limits,
    };
    let mut budgets = Budgets::new(limits);
    let blocks = read_body(&text, &ctx, &mut budgets)?;
    Ok(Docx {
        title,
        blocks: tidy(blocks),
    })
}

/// The part a relationship of type `kind` names; the conventional path only
/// when the main part has no relationships at all.
fn part_of(rels: &Relationships, kind: &str, fallback: &str) -> Option<String> {
    rels.of_type(kind)
        .find(|r| !r.external)
        .map(|r| r.target.clone())
        .or_else(|| rels.is_empty().then(|| fallback.to_owned()))
}

/// Merges consecutive code paragraphs into one code block and consecutive
/// quote paragraphs into one quote, and drops page breaks at the ends.
fn tidy(blocks: Vec<ParsedBlock>) -> Vec<ParsedBlock> {
    let mut out: Vec<ParsedBlock> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match (out.last_mut(), block) {
            (
                Some(ParsedBlock::CodeBlock { code, .. }),
                ParsedBlock::CodeBlock { code: more, .. },
            ) => {
                code.push('\n');
                code.push_str(&more);
            }
            (Some(ParsedBlock::Quote { blocks }), ParsedBlock::Quote { blocks: more }) => {
                blocks.extend(more);
            }
            (_, block) => out.push(block),
        }
    }
    for block in &mut out {
        if let ParsedBlock::CodeBlock { code, .. } = block {
            let lines: Vec<&str> = code.lines().collect();
            let start = lines.iter().position(|l| !l.trim().is_empty());
            let end = lines.iter().rposition(|l| !l.trim().is_empty());
            *code = match (start, end) {
                (Some(s), Some(e)) => lines.get(s..=e).map(|l| l.join("\n")).unwrap_or_default(),
                _ => String::new(),
            };
        }
    }
    out.retain(|b| !matches!(b, ParsedBlock::CodeBlock { code, .. } if code.is_empty()));
    out.dedup_by(|a, b| *a == ParsedBlock::PageBreak && *b == ParsedBlock::PageBreak);
    let leading = out
        .iter()
        .take_while(|b| **b == ParsedBlock::PageBreak)
        .count();
    out.drain(..leading);
    while out.last() == Some(&ParsedBlock::PageBreak) {
        out.pop();
    }
    out
}
