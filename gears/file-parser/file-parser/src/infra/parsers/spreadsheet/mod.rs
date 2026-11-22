//! XLSX, XLSM, XLSB and legacy XLS workbooks.
//!
//! Each worksheet becomes one level-2 heading (the sheet name, with
//! "(hidden)" or "(very hidden)" for sheets Excel does not show) and one table
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
mod compound;
mod numfmt;
mod shared;
mod xls;
mod xlsb;
mod xlsx;

use std::path::Path;

use bytes::Bytes;

use crate::domain::error::{DomainError, ParseLimit};
use crate::domain::ir::{Inline, ParsedBlock, TableBlock, TableCell, TableRow};
use crate::domain::mime_table;

use super::backend::{DocumentFormat, DocumentParser, ReadDocument, run_blocking};
use super::limits::{BLOCK_COST, ParseLimits, TableBudget, TextBudget, limit_exceeded};
use super::ooxml::{self, Archive};

/// Signature of an OLE compound file (legacy `.xls`).
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Spreadsheets: XLSX, XLSM, XLSB and legacy XLS, told apart by content.
pub struct Spreadsheet;

/// Spreadsheet parser.
pub type SpreadsheetParser = DocumentParser<Spreadsheet>;

impl DocumentFormat for Spreadsheet {
    const ID: &'static str = "spreadsheet";
    const EXTENSIONS: &'static [&'static str] = &["xlsx", "xls", "xlsm", "xlsb"];

    async fn read(
        bytes: Bytes,
        _content_type: Option<String>,
        limits: ParseLimits,
    ) -> Result<ReadDocument, DomainError> {
        let workbook = run_blocking("spreadsheet", move || read_workbook(bytes, &limits)).await?;
        Ok(ReadDocument::new(
            workbook.format.mime(),
            workbook.title,
            workbook.blocks,
        ))
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
            Self::Xls => mime_table::XLS,
            Self::Xlsx => mime_table::XLSX,
            Self::Xlsm => mime_table::XLSM,
            Self::Xlsb => mime_table::XLSB,
        }
    }
}

struct Workbook {
    format: Format,
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

/// Whether Excel shows a sheet tab. Hidden sheets are still read; the heading
/// says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Visibility {
    #[default]
    Visible,
    /// The user can unhide it.
    Hidden,
    /// Only a macro can unhide it.
    VeryHidden,
}

impl Visibility {
    /// The sheet-state number BIFF8 and BIFF12 use (`hsState`).
    fn from_state(state: u8) -> Self {
        match state {
            1 => Self::Hidden,
            2 => Self::VeryHidden,
            _ => Self::Visible,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Visible => "",
            Self::Hidden => " (hidden)",
            Self::VeryHidden => " (very hidden)",
        }
    }
}

/// Collects the blocks of a workbook under the document-wide budgets.
pub(super) struct Sink {
    budget: TableBudget,
    text: TextBudget,
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
            text: TextBudget::new(limits),
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
        self.text.charge(text.len())?;
        self.copied = self.copied.saturating_add(text.len());
        if self.copied > self.max_copied {
            return Err(limit_exceeded(
                ParseLimit::MaxTextBytes,
                format!(
                    "cells copy more than {} bytes of text, skipped sheets included",
                    self.max_copied
                ),
            ));
        }
        sheet.text_bytes += text.len();
        Ok(sheet.push(row, col, text))
    }

    /// Appends the heading and table (or a skip note) for one sheet.
    fn finish_sheet(
        &mut self,
        name: &str,
        visibility: Visibility,
        sheet: SheetCells,
    ) -> Result<(), DomainError> {
        let heading = format!("{name}{}", visibility.suffix());
        // The heading costs text too, and it and the table (or the skip note)
        // are a block each.
        self.text
            .charge(heading.len().saturating_add(2 * BLOCK_COST))?;
        let text_bytes = sheet.text_bytes;
        if !emit_sheet(&heading, sheet, &mut self.budget, &mut self.blocks) {
            self.text.refund(text_bytes);
            let note = skipped_note_text(name, self.budget.remaining());
            self.text.charge(note.len())?;
            self.blocks.push(ParsedBlock::Paragraph {
                inlines: vec![Inline::plain(note)],
            });
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
    let root = archive.read_rels("")?;
    // One lookup decides the format and is what the reader then opens.
    let workbook_part = ooxml::main_part(&archive, &root, &["xl/workbook.bin", "xl/workbook.xml"])
        .ok_or_else(|| DomainError::parse_error("Failed to parse spreadsheet: no workbook part"))?;
    let title = ooxml::package_title(&mut archive, &root)?;
    if is_binary(&workbook_part) {
        xlsb::read(archive, &workbook_part, limits, &mut sink)?;
        return Ok(Workbook {
            format: Format::Xlsb,
            title,
            blocks: sink.blocks,
        });
    }
    let info = xlsx::read(archive, &workbook_part, limits, &mut sink)?;
    Ok(Workbook {
        format: if info.macro_enabled {
            Format::Xlsm
        } else {
            Format::Xlsx
        },
        title,
        blocks: sink.blocks,
    })
}

/// Whether the workbook part is binary (XLSB).
fn is_binary(workbook_part: &str) -> bool {
    Path::new(workbook_part)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
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

/// Appends the heading and table for one sheet; `false` when the sheet's
/// cells were skipped, in which case the caller appends the skip note.
fn emit_sheet(
    heading: &str,
    sheet: SheetCells,
    budget: &mut TableBudget,
    out: &mut Vec<ParsedBlock>,
) -> bool {
    out.push(ParsedBlock::Heading {
        level: 2,
        inlines: vec![Inline::plain(heading)],
    });
    let (Some((r0, r1)), Some((c0, c1))) = (sheet.rows, sheet.cols) else {
        return !sheet.overflow;
    };
    let rows = usize::try_from(r1 - r0).map_or(usize::MAX, |n| n.saturating_add(1));
    let cols = usize::try_from(c1 - c0).map_or(usize::MAX, |n| n.saturating_add(1));
    // The cells were gathered by the sink, not built into a grid: nothing to
    // spend for them.
    if !budget.admit("spreadsheet", rows, cols, 0, sheet.overflow) {
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

fn skipped_note_text(name: &str, remaining: usize) -> String {
    format!(
        "Sheet \"{name}\" skipped: it does not fit the remaining limit of {remaining} table cells"
    )
}

#[cfg(test)]
mod tests;
