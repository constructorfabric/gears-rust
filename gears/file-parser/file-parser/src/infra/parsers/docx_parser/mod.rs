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

use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::ParsedBlock;
use crate::domain::mime_table;

use super::backend::{DocumentFormat, DocumentParser, ReadDocument, run_blocking};
use super::limits::{Budgets, ParseLimits};
use super::ooxml::{self, Archive, Relationships};
use body::{DocContext, read_body};
use styles::{Numbering, Styles, read_numbering, read_styles};

/// DOCX: body text, tables and pictures.
pub struct Docx;

/// DOCX parser.
pub type DocxParser = DocumentParser<Docx>;

impl DocumentFormat for Docx {
    const ID: &'static str = "docx";
    const EXTENSIONS: &'static [&'static str] = &["docx"];

    async fn read(
        bytes: Bytes,
        _content_type: Option<String>,
        limits: ParseLimits,
    ) -> Result<ReadDocument, DomainError> {
        let doc = run_blocking("DOCX", move || read_docx(bytes, &limits)).await?;
        Ok(ReadDocument::new(mime_table::DOCX, doc.title, doc.blocks))
    }
}

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse DOCX: {detail}"))
}

fn tolerate<T: Default>(result: Result<T, DomainError>, what: &str) -> Result<T, DomainError> {
    ooxml::tolerate(result, "DOCX", what)
}

struct DocxDocument {
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

fn read_docx(bytes: Bytes, limits: &ParseLimits) -> Result<DocxDocument, DomainError> {
    let mut archive = Archive::open(bytes, "DOCX", limits)?;
    let root = archive.read_rels("")?;
    let main = ooxml::main_part(&archive, &root, &["word/document.xml"])
        .ok_or_else(|| parse_error("no document part"))?;
    let title = ooxml::package_title(&mut archive, &root)?;

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
    Ok(DocxDocument {
        title,
        blocks: tidy(blocks),
    })
}

/// The part a relationship of type `kind` names; the conventional path only
/// when the main part has no relationships at all.
fn part_of(rels: &Relationships, kind: &str, fallback: &str) -> Option<String> {
    rels.internal(kind)
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
