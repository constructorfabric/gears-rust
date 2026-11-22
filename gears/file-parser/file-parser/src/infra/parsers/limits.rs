//! Resource limits shared by the document parsers.
//!
//! Every input is attacker-controlled: a few kilobytes of ZIP or XML can ask for
//! gigabytes of memory or unbounded recursion. Each parser checks the limits it
//! can hit and fails with an error that names the limit instead of letting the
//! process run out of memory.

use crate::domain::error::DomainError;

const MIB: u64 = 1024 * 1024;

/// Upper bounds on the work one document may cause.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Element nesting depth in XML and HTML.
    pub max_nesting_depth: usize,
    /// Table cells across the whole document. Each table is charged its full
    /// rows x columns area, since that is what the output holds.
    pub max_table_cells: usize,
    /// Columns in one table.
    pub max_table_columns: usize,
    /// Sheets (or slides) in one workbook (or presentation).
    pub max_sheets: usize,
    /// Pages of one PDF.
    pub max_pdf_pages: usize,
    /// Text extracted from one document.
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
            max_pdf_pages: 2_000,
            max_text_bytes: 100 * 1024 * 1024,
        }
    }
}

/// Error for a document that exceeds `limit`.
pub(crate) fn limit_exceeded(limit: &str, detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("document exceeds parser limit `{limit}`: {detail}"))
}

/// The document-wide table-cell allowance ([`ParseLimits::max_table_cells`]).
#[derive(Debug)]
pub(crate) struct TableBudget {
    remaining: usize,
    max_columns: usize,
}

impl TableBudget {
    pub(crate) fn new(limits: &ParseLimits) -> Self {
        Self {
            remaining: limits.max_table_cells,
            max_columns: limits.max_table_columns,
        }
    }

    /// Cells still allowed.
    pub(crate) fn remaining(&self) -> usize {
        self.remaining
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
                "max_nesting_depth",
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
