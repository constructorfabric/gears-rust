//! Resource limits shared by the document parsers.
//!
//! Every input is attacker-controlled: a few kilobytes of ZIP or XML can ask for
//! gigabytes of memory or unbounded recursion. Each parser checks the limits it
//! can hit and fails with an error that names the limit instead of letting the
//! process run out of memory.

use std::time::Duration;

use crate::domain::error::{DomainError, ParseLimit};
use crate::domain::ir::{Inline, ParsedBlock};

const MIB: u64 = 1024 * 1024;

/// Upper bounds on the work one document may cause. Start from
/// [`ParseLimits::default`] and change fields; new limits may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseLimits {
    /// Entries in a ZIP container (DOCX, PPTX, XLSX).
    pub max_zip_entries: usize,
    /// Decompressed size of one ZIP entry.
    pub max_entry_bytes: u64,
    /// Decompressed size of all entries of one container, declared and read.
    pub max_total_uncompressed_bytes: u64,
    /// Decompressed / compressed ratio above which an entry is treated as a ZIP bomb.
    pub max_compression_ratio: u64,
    /// Entries smaller than this skip the ratio check: tiny, repetitive XML parts
    /// legitimately compress far better than 100:1.
    pub ratio_check_floor_bytes: u64,
    /// Element nesting depth in XML parts. HTML keeps its own, smaller caps
    /// (its tree and converter flatten deeper content instead of failing).
    pub max_nesting_depth: usize,
    /// Table cells across the whole document. Each table is charged its full
    /// rows x columns area, since that is what the output holds.
    pub max_table_cells: usize,
    /// Columns in one table.
    pub max_table_columns: usize,
    /// Sheets (or slides) in one workbook (or presentation).
    pub max_sheets: usize,
    /// Elements and text nodes of one HTML document.
    pub max_html_nodes: usize,
    /// Pages of one PDF.
    pub max_pdf_pages: usize,
    /// Characters pdfium reports on one PDF page.
    pub max_pdf_chars_per_page: usize,
    /// Wall-clock time one PDF may spend on the PDF engine reading its
    /// pages, and the longest it waits for the engine (then it is turned away
    /// as busy). It is checked between pdfium calls; a single call cannot be
    /// interrupted, so a hostile document can keep the engine busy past it;
    /// the engine's part of the answer still comes within twice this plus
    /// 2 s (see the PDF engine module docs). Arranging the lines into blocks
    /// follows on the blocking pool and is bounded by the page, character and
    /// text limits, not by this.
    pub pdf_timeout: Duration,
    /// Text extracted from one document, plus [`BLOCK_COST`] for each block,
    /// inline run and PDF line produced: many tiny pieces of text take far
    /// more memory than their bytes, and this bounds that memory too.
    pub max_text_bytes: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_zip_entries: 10_000,
            max_entry_bytes: 64 * MIB,
            max_total_uncompressed_bytes: 500 * MIB,
            max_compression_ratio: 100,
            ratio_check_floor_bytes: MIB,
            max_nesting_depth: 1024,
            max_table_cells: 100_000,
            max_table_columns: 16_384,
            max_sheets: 1_000,
            max_html_nodes: 500_000,
            max_pdf_pages: 2_000,
            max_pdf_chars_per_page: 200_000,
            pdf_timeout: Duration::from_secs(60),
            max_text_bytes: 100 * 1024 * 1024,
        }
    }
}

/// Longest document title kept, in characters.
pub(crate) const MAX_TITLE_CHARS: usize = 512;

/// Raw title text collected before whitespace is collapsed and the title cut
/// to [`MAX_TITLE_CHARS`].
const MAX_RAW_TITLE_BYTES: usize = 16 * 1024;

/// Appends `text` to a raw title until it holds [`MAX_RAW_TITLE_BYTES`]; a
/// title as large as the document is not kept.
pub(crate) fn push_title_text(title: &mut String, text: &str) {
    let room = MAX_RAW_TITLE_BYTES.saturating_sub(title.len());
    title.push_str(&text[..text.floor_char_boundary(room)]);
}

/// A raw title with runs of ASCII whitespace (line breaks included) turned
/// into single spaces, as HTML does; other whitespace inside the title is
/// kept. Trimmed of any whitespace at both ends and cut to
/// [`MAX_TITLE_CHARS`]; `None` when nothing is left.
pub(crate) fn finish_title(raw: &str) -> Option<String> {
    let collapsed = raw.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = collapsed.trim().chars().take(MAX_TITLE_CHARS).collect();
    (!title.is_empty()).then_some(title)
}

/// Error for a document that exceeds `limit`.
pub(crate) fn limit_exceeded(limit: ParseLimit, detail: impl std::fmt::Display) -> DomainError {
    DomainError::limit_exceeded(limit, detail.to_string())
}

/// Whether `error` came from [`limit_exceeded`]. Parsers that tolerate a
/// malformed optional part still stop on these.
pub(crate) fn is_limit(error: &DomainError) -> bool {
    matches!(error, DomainError::LimitExceeded { .. })
}

/// What each block, inline run and PDF line costs from the text budget on
/// top of its text: about the heap it takes (an enum, a `Vec` and a `String`
/// allocation). Real documents average hundreds of text bytes per block, so
/// this matters only to inputs built from millions of empty or 1-byte pieces.
pub const BLOCK_COST: usize = 64;

/// The document-wide text allowance ([`ParseLimits::max_text_bytes`]).
#[derive(Debug)]
pub(crate) struct TextBudget {
    left: usize,
    max: usize,
}

impl TextBudget {
    pub(crate) fn new(limits: &ParseLimits) -> Self {
        Self {
            left: limits.max_text_bytes,
            max: limits.max_text_bytes,
        }
    }

    /// Takes `bytes` of text from the allowance, or fails once it is spent.
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), DomainError> {
        self.left = self.left.checked_sub(bytes).ok_or_else(|| {
            limit_exceeded(
                ParseLimit::MaxTextBytes,
                format!("document text exceeds {} bytes", self.max),
            )
        })?;
        Ok(())
    }

    /// Charges one block, inline run or PDF line ([`BLOCK_COST`]).
    pub(crate) fn charge_block(&mut self) -> Result<(), DomainError> {
        self.charge(BLOCK_COST)
    }

    /// Gives back text that was charged but is not kept after all, never
    /// more than the allowance.
    pub(crate) fn refund(&mut self, bytes: usize) {
        self.left = self.left.saturating_add(bytes).min(self.max);
    }
}

/// The table-cell and text allowances of one document.
pub(crate) struct Budgets {
    pub(crate) tables: TableBudget,
    pub(crate) text: TextBudget,
}

impl Budgets {
    pub(crate) fn new(limits: &ParseLimits) -> Self {
        Self {
            tables: TableBudget::new(limits),
            text: TextBudget::new(limits),
        }
    }
}

/// The paragraph that stands in for a table over the cell budget, charged
/// to `text` as a block with its text: once the cell budget is spent, every
/// further table in the document becomes one.
pub(crate) fn table_note(
    remaining: usize,
    text: &mut TextBudget,
) -> Result<ParsedBlock, DomainError> {
    let note = table_note_text(remaining);
    text.charge(note.len().saturating_add(BLOCK_COST))?;
    Ok(ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(note)],
    })
}

/// The block [`table_note`] puts out.
#[cfg(test)]
pub(crate) fn skipped_table_note(remaining: usize) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(table_note_text(remaining))],
    }
}

fn table_note_text(remaining: usize) -> String {
    format!("Table skipped: it does not fit the remaining limit of {remaining} table cells")
}

/// The document-wide table-cell allowance ([`ParseLimits::max_table_cells`]).
#[derive(Debug)]
pub(crate) struct TableBudget {
    remaining: usize,
    max_columns: usize,
    /// A skip was logged at warn; later ones go to debug, so a document of
    /// many small tables cannot flood the log.
    warned: bool,
}

impl TableBudget {
    pub(crate) fn new(limits: &ParseLimits) -> Self {
        Self {
            remaining: limits.max_table_cells,
            max_columns: limits.max_table_columns,
            warned: false,
        }
    }

    /// Cells still allowed.
    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }

    /// Takes `cells` from the allowance (saturating), e.g. for a table that
    /// was built and then skipped.
    pub(crate) fn spend(&mut self, cells: usize) {
        self.remaining = self.remaining.saturating_sub(cells);
    }

    /// Widest table allowed.
    pub(crate) fn max_columns(&self) -> usize {
        self.max_columns
    }

    /// Decides whether a table that was built is kept. A table cut short while
    /// it was built (`overflow`), too wide, or over the remaining cells is
    /// skipped: the `built` cells are spent anyway, so a run of oversized
    /// tables cannot each build up to the whole budget, and the skip is
    /// logged (the first skip of a document at warn). `source` names the
    /// format in the log; it is a literal, so no document text (a sheet name,
    /// say) reaches the log.
    pub(crate) fn admit(
        &mut self,
        source: &'static str,
        rows: usize,
        columns: usize,
        built: usize,
        overflow: bool,
    ) -> bool {
        if !overflow && self.try_charge(rows, columns) {
            return true;
        }
        self.spend(built);
        if self.warned {
            tracing::debug!(
                %source,
                rows,
                columns,
                overflow,
                remaining = self.remaining,
                "table skipped: over the table-cell limit"
            );
        } else {
            self.warned = true;
            tracing::warn!(
                %source,
                rows,
                columns,
                overflow,
                remaining = self.remaining,
                "table skipped: over the table-cell limit; later skips are logged at debug"
            );
        }
        false
    }

    /// Charges a `rows` x `columns` table if it fits; returns `false` and charges
    /// nothing otherwise (too wide, or more cells than remain).
    pub(crate) fn try_charge(&mut self, rows: usize, columns: usize) -> bool {
        if columns > self.max_columns {
            return false;
        }
        match rows.checked_mul(columns) {
            Some(cells) if cells <= self.remaining => {
                self.remaining -= cells;
                true
            }
            _ => false,
        }
    }
}

/// Tracks element nesting while walking XML or a DOM.
#[derive(Debug)]
pub(crate) struct DepthGuard {
    depth: usize,
    max: usize,
}

impl DepthGuard {
    pub(crate) fn new(limits: &ParseLimits) -> Self {
        Self {
            depth: 0,
            max: limits.max_nesting_depth,
        }
    }

    pub(crate) fn enter(&mut self) -> Result<(), DomainError> {
        self.depth += 1;
        if self.depth > self.max {
            return Err(limit_exceeded(
                ParseLimit::MaxNestingDepth,
                format!("nesting deeper than {}", self.max),
            ));
        }
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_cut_on_a_character_boundary() {
        // 16 KiB is not a multiple of 3: the cut falls inside a 3-byte
        // character and is moved back to its start.
        let mut title = String::new();
        push_title_text(&mut title, &"\u{20ac}".repeat(10_000));
        assert_eq!(title.len(), MAX_RAW_TITLE_BYTES - MAX_RAW_TITLE_BYTES % 3);
        assert!(title.ends_with('\u{20ac}'));
        // One byte of room: a 3-byte character does not fit.
        push_title_text(&mut title, "\u{20ac}");
        assert_eq!(title.len(), MAX_RAW_TITLE_BYTES - MAX_RAW_TITLE_BYTES % 3);
    }

    #[test]
    fn a_title_is_collapsed_to_one_line() {
        assert_eq!(
            finish_title(" a\n\n---\r\nlanguage: x \t b ").as_deref(),
            Some("a --- language: x b")
        );
        assert_eq!(finish_title(" \n\t"), None);
        assert_eq!(
            finish_title(&"x".repeat(MAX_TITLE_CHARS + 5)).map(|t| t.len()),
            Some(MAX_TITLE_CHARS)
        );
    }

    #[test]
    fn blocks_and_table_notes_are_charged_beyond_their_text() {
        let with = |max_text_bytes| ParseLimits {
            max_text_bytes,
            ..ParseLimits::default()
        };
        let mut budget = TextBudget::new(&with(2 * BLOCK_COST));
        budget.charge_block().unwrap();
        budget.charge_block().unwrap();
        assert!(budget.charge_block().is_err());

        let note = table_note_text(0).len() + BLOCK_COST;
        let mut budget = TextBudget::new(&with(note));
        assert_eq!(table_note(0, &mut budget).unwrap(), skipped_table_note(0));
        let mut budget = TextBudget::new(&with(note - 1));
        assert!(table_note(0, &mut budget).is_err());
    }

    #[test]
    fn table_budget_charges_area_and_refuses_what_does_not_fit() {
        let limits = ParseLimits {
            max_table_cells: 10,
            max_table_columns: 4,
            ..ParseLimits::default()
        };
        let mut budget = TableBudget::new(&limits);
        assert!(budget.try_charge(2, 3));
        assert_eq!(budget.remaining(), 4);
        assert!(!budget.try_charge(5, 1), "more cells than remain");
        assert!(!budget.try_charge(1, 5), "wider than the column limit");
        assert!(!budget.try_charge(usize::MAX, 2), "overflowing area");
        assert_eq!(budget.remaining(), 4, "refused tables cost nothing");
        assert!(budget.try_charge(2, 2));
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn depth_guard_rejects_past_limit_and_recovers_on_leave() {
        let limits = ParseLimits {
            max_nesting_depth: 2,
            ..ParseLimits::default()
        };
        let mut guard = DepthGuard::new(&limits);
        guard.enter().unwrap();
        guard.enter().unwrap();
        assert!(guard.enter().is_err());
        guard.leave();
        guard.leave();
        guard.enter().unwrap();
    }
}
