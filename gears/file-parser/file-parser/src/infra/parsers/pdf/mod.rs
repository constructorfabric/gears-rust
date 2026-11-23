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
mod worker;

pub(crate) use engine::{DEFAULT_MAX_QUEUED, check_engine, configure_queue, longest_wait};

use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::ParsedBlock;
use crate::domain::mime_table;

use super::backend::{DocumentFormat, DocumentParser, ReadDocument, run_blocking};
use super::limits::{ParseLimits, TextBudget};

/// PDF, read through libpdfium.
pub struct Pdf;

/// PDF parser.
pub type PdfParser = DocumentParser<Pdf>;

impl DocumentFormat for Pdf {
    const ID: &'static str = "pdf";
    const EXTENSIONS: &'static [&'static str] = &["pdf"];

    async fn read(
        bytes: Bytes,
        _content_type: Option<String>,
        limits: ParseLimits,
    ) -> Result<ReadDocument, DomainError> {
        let pdf = read_pdf(bytes, limits).await?;
        Ok(ReadDocument::new(mime_table::PDF, pdf.title, pdf.blocks))
    }
}

struct PdfDocument {
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

/// What the engine thread hands back: the lines of every page, and the text
/// budget they were charged to.
struct PdfLines {
    title: Option<String>,
    pages: Vec<structure::Page>,
    budget: TextBudget,
}

/// Reads `bytes`: glyphs and lines on the PDF engine thread, then structure
/// on the blocking pool. Structure is plain Rust over the whole document, so
/// it does not hold the one pdfium thread the next document waits for.
// cancel-safe: as `engine::run` while reading; dropping it during structure
// discards the result of a bounded computation.
async fn read_pdf(bytes: Bytes, limits: ParseLimits) -> Result<PdfDocument, DomainError> {
    let run_limits = limits.clone();
    let lines = engine::run(&run_limits, move |engine| collect(engine, &bytes, &limits)).await?;
    run_blocking("PDF", move || {
        let PdfLines {
            title,
            pages,
            mut budget,
        } = lines;
        let blocks = structure::blocks(pages, &mut budget)?;
        Ok(PdfDocument { title, blocks })
    })
    .await
}

fn collect(
    engine: &engine::Engine<'_>,
    bytes: &[u8],
    limits: &ParseLimits,
) -> Result<PdfLines, DomainError> {
    let mut budget = TextBudget::new(limits);
    let mut pages = Vec::new();
    // Lines, not glyphs, are kept across pages: structure needs the whole
    // document (body size, running headers), and lines are charged as text.
    let info = engine.read_document(bytes, limits, &mut |page| {
        let lines = layout::page_lines(page.glyphs, &mut budget)?;
        pages.push(structure::Page {
            lines,
            area: page.area,
        });
        Ok(())
    })?;
    Ok(PdfLines {
        title: info.title,
        pages,
        budget,
    })
}
