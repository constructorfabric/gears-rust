//! XLSX, XLSM, XLSB and legacy XLS workbooks.
//!
//! Each worksheet becomes one level-2 heading (the sheet name) and one table
//! over the bounding box of its non-empty cells; the first row is the header.
//! A sheet whose table does not fit the remaining
//! [`ParseLimits::max_table_cells`] is replaced by a note, and the text copied
//! into cells is held to [`ParseLimits::max_text_bytes`].
//!
//! Each format has its own reader ([`xlsx`], [`xlsb`], [`xls`]) that reads cell
//! values only and never sizes anything from a count or dimension the file
//! declares. A general-purpose spreadsheet library was tried and dropped:
//! calamine allocates dense ranges and buffers from declared sizes, decodes
//! every formula eagerly, and reuses stale buffers on short records, so small
//! hostile files could exhaust memory, the stack or the CPU.

mod cells;
mod numfmt;
mod shared;
mod xls;
mod xlsb;
mod xlsx;

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ir::{
    DocumentBuilder, Inline, ParsedBlock, ParsedDocument, ParsedSource, TableBlock, TableCell,
    TableRow,
};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::{ParseLimits, TableBudget, limit_exceeded};
use super::ooxml::Archive;

/// Ceiling applied by [`SpreadsheetParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default; `Gear::init` passes the configured value.
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

const SUPPORTED_EXTENSIONS: &[&str] = &["xlsx", "xls", "xlsm", "xlsb"];

/// Signature of an OLE compound file (legacy `.xls`).
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Spreadsheet parser.
pub struct SpreadsheetParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl SpreadsheetParser {
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
        let workbook = tokio::task::spawn_blocking(move || read_workbook(bytes, &limits))
            .await
            .map_err(|e| DomainError::parse_error(format!("Spreadsheet task failed: {e}")))??;

        let mut builder = DocumentBuilder::new(source)
            .content_type(workbook.format.mime())
            .blocks(workbook.blocks);
        if let Some(title) = workbook.title.as_deref().or(filename) {
            builder = builder.title(title);
        }
        if let Some(filename) = filename {
            builder = builder.original_filename(filename);
        }
        Ok(builder.build())
    }
}

impl Default for SpreadsheetParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for SpreadsheetParser {
    fn id(&self) -> &'static str {
        "spreadsheet"
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

/// Workbook format, taken from the content rather than the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Xls,
    Xlsx,
    Xlsm,
    Xlsb,
}

impl Format {
    fn mime(self) -> &'static str {
        match self {
            Self::Xls => "application/vnd.ms-excel",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xlsm => "application/vnd.ms-excel.sheet.macroEnabled.12",
            Self::Xlsb => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        }
    }
}

struct Workbook {
    format: Format,
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

/// Collects the blocks of a workbook under the document-wide budgets.
pub(super) struct Sink {
    budget: TableBudget,
    text_left: usize,
    max_text: usize,
    /// Text copied into cells so far, refunds included: skipped sheets give
    /// their text back to the budget, but not to this ceiling, so the total
    /// copying work stays bounded too.
    copied: usize,
    max_copied: usize,
    blocks: Vec<ParsedBlock>,
}

impl Sink {
    fn new(limits: &ParseLimits) -> Self {
        Self {
            budget: TableBudget::new(limits),
            text_left: limits.max_text_bytes,
            max_text: limits.max_text_bytes,
            copied: 0,
            max_copied: limits.max_text_bytes.saturating_mul(4),
            blocks: Vec::new(),
        }
    }

    /// An empty sheet that may hold as many cells as the budget still allows.
    fn new_sheet(&self) -> SheetCells {
        SheetCells::new(self.budget.remaining())
    }

    /// Adds a cell to `sheet`; `Ok(false)` once the sheet can no longer fit,
    /// an error once the document's text limit is spent. The charge is
    /// returned if the sheet is skipped in the end.
    fn push(
        &mut self,
        sheet: &mut SheetCells,
        row: u32,
        col: u32,
        text: String,
    ) -> Result<bool, DomainError> {
        self.charge_text(text.len())?;
        self.copied = self.copied.saturating_add(text.len());
        if self.copied > self.max_copied {
            return Err(limit_exceeded(
                "max_text_bytes",
                format!(
                    "cells copy more than {} bytes of text, skipped sheets included",
                    self.max_copied
                ),
            ));
        }
        sheet.text_bytes += text.len();
        Ok(sheet.push(row, col, text))
    }

    fn charge_text(&mut self, bytes: usize) -> Result<(), DomainError> {
        self.text_left = self.text_left.checked_sub(bytes).ok_or_else(|| {
            limit_exceeded(
                "max_text_bytes",
                format!("document text exceeds {} bytes", self.max_text),
            )
        })?;
        Ok(())
    }

    /// Appends the heading and table (or a skip note) for one sheet.
    fn finish_sheet(&mut self, name: &str, sheet: SheetCells) -> Result<(), DomainError> {
        // The heading, and possibly a note quoting the name, cost text too.
        self.charge_text(name.len().saturating_mul(2))?;
        let text_bytes = sheet.text_bytes;
        if !emit_sheet(name, sheet, &mut self.budget, &mut self.blocks) {
            self.text_left = self.text_left.saturating_add(text_bytes);
        }
        Ok(())
    }
}

fn read_workbook(bytes: Bytes, limits: &ParseLimits) -> Result<Workbook, DomainError> {
    let mut sink = Sink::new(limits);
    if bytes.starts_with(&CFB_MAGIC) {
        xls::read(&bytes, limits, &mut sink)?;
        return Ok(Workbook {
            format: Format::Xls,
            title: None,
            blocks: sink.blocks,
        });
    }
    if !bytes.starts_with(b"PK") {
        return Err(DomainError::parse_error(
            "Failed to parse spreadsheet: not an Excel workbook",
        ));
    }
    let mut archive = Archive::open(bytes, "spreadsheet", limits)?;
    if is_xlsb(&mut archive)? {
        let title = xlsb::read(archive, limits, &mut sink)?;
        return Ok(Workbook {
            format: Format::Xlsb,
            title,
            blocks: sink.blocks,
        });
    }
    let info = xlsx::read(archive, limits, &mut sink)?;
    Ok(Workbook {
        format: if info.macro_enabled {
            Format::Xlsm
        } else {
            Format::Xlsx
        },
        title: info.title,
        blocks: sink.blocks,
    })
}

/// Whether the package's main part is a binary (XLSB) workbook, judged from
/// the officeDocument relationship like every other part lookup.
fn is_xlsb(archive: &mut Archive) -> Result<bool, DomainError> {
    let root = archive.read_rels("")?;
    let target = root
        .of_type("officeDocument")
        .find(|r| !r.external)
        .map(|r| r.target.clone());
    Ok(match target {
        Some(target) => Path::new(&target)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bin")),
        None => archive.contains("xl/workbook.bin"),
    })
}

/// The non-empty cells of one sheet, collected sparsely.
pub(super) struct SheetCells {
    cells: Vec<(u32, u32, String)>,
    /// Text charged to the document for these cells, returned on skip.
    text_bytes: usize,
    /// Cells beyond this many cannot fit the remaining budget anyway.
    max_cells: usize,
    overflow: bool,
    rows: Option<(u32, u32)>,
    cols: Option<(u32, u32)>,
}

impl SheetCells {
    fn new(max_cells: usize) -> Self {
        Self {
            cells: Vec::new(),
            text_bytes: 0,
            max_cells,
            overflow: false,
            rows: None,
            cols: None,
        }
    }

    /// Adds a cell; returns `false` once the sheet can no longer fit.
    fn push(&mut self, row: u32, col: u32, text: String) -> bool {
        if self.cells.len() >= self.max_cells {
            self.overflow = true;
            return false;
        }
        self.rows = Some(
            self.rows
                .map_or((row, row), |(lo, hi)| (lo.min(row), hi.max(row))),
        );
        self.cols = Some(
            self.cols
                .map_or((col, col), |(lo, hi)| (lo.min(col), hi.max(col))),
        );
        self.cells.push((row, col, text));
        true
    }
}

/// Appends the heading and table (or a skip note) for one sheet; `false` when
/// the sheet's cells were skipped.
fn emit_sheet(
    name: &str,
    sheet: SheetCells,
    budget: &mut TableBudget,
    out: &mut Vec<ParsedBlock>,
) -> bool {
    out.push(ParsedBlock::Heading {
        level: 2,
        inlines: vec![Inline::plain(name)],
    });
    let (Some((r0, r1)), Some((c0, c1))) = (sheet.rows, sheet.cols) else {
        if sheet.overflow {
            out.push(skipped_note(name, budget.remaining()));
            return false;
        }
        return true;
    };
    let rows = usize::try_from(r1 - r0).map_or(usize::MAX, |n| n.saturating_add(1));
    let cols = usize::try_from(c1 - c0).map_or(usize::MAX, |n| n.saturating_add(1));
    if sheet.overflow || !budget.try_charge(rows, cols) {
        let shown: String = name.chars().take(64).collect();
        tracing::warn!(
            sheet = %shown,
            rows,
            cols,
            "sheet skipped: over the table-cell limit"
        );
        out.push(skipped_note(name, budget.remaining()));
        return false;
    }

    // rows * cols fits the budget, so this allocation is bounded. A later cell
    // at the same position replaces an earlier one.
    let mut grid: Vec<Option<String>> = vec![None; rows * cols];
    for (row, col, text) in sheet.cells {
        let (Ok(r), Ok(c)) = (usize::try_from(row - r0), usize::try_from(col - c0)) else {
            continue;
        };
        if let Some(slot) = grid.get_mut(r * cols + c) {
            *slot = Some(text);
        }
    }
    let mut grid = grid.into_iter();
    let table_rows = (0..rows)
        .map(|row_idx| TableRow {
            is_header: row_idx == 0,
            cells: grid
                .by_ref()
                .take(cols)
                .map(|text| TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain(text.unwrap_or_default())],
                    }],
                })
                .collect(),
        })
        .collect();
    out.push(ParsedBlock::Table(TableBlock { rows: table_rows }));
    true
}

fn skipped_note(name: &str, remaining: usize) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(format!(
            "[Sheet \"{name}\" skipped: it does not fit the remaining limit of {remaining} table cells]"
        ))],
    }
}

#[cfg(test)]
mod tests;
