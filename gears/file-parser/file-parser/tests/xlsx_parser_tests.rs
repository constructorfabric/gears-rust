#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

// NOTE: Very large sheet tests (memory behavior with millions of cells) are better suited
// for E2E tests due to test file size and execution time. Unit tests here cover correctness
// of parsing features like merged cells and formulas.

use file_parser::domain::parser::FileParserBackend;
use file_parser::infra::parsers::SpreadsheetParser;
use std::path::PathBuf;

mod common;

fn get_test_file_path(filename: &str) -> PathBuf {
    common::fixture_path("xlsx", filename)
}

#[tokio::test]
async fn test_xlsx_parser_basic_info() {
    let parser = SpreadsheetParser::new();

    assert_eq!(parser.id(), "spreadsheet");
    assert!(
        parser.supported_extensions().contains(&"xlsx"),
        "SpreadsheetParser should support xlsx extension"
    );
    assert!(
        parser.supported_extensions().contains(&"xls"),
        "SpreadsheetParser should support xls extension"
    );
}

#[tokio::test]
async fn test_xlsx_parser_with_simple_file() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("simple_data.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse XLSX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");

    // Verify document metadata
    assert!(document.meta.original_filename.is_some());
    assert_eq!(
        document.meta.original_filename.as_deref(),
        Some("simple_data.xlsx")
    );
    assert_eq!(
        document.meta.content_type.as_deref(),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    );
}

#[tokio::test]
async fn test_xlsx_parser_with_multisheet_file() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("multi_sheet.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse multi-sheet XLSX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");

    // Count heading blocks (one per sheet)
    let heading_count = document
        .blocks
        .iter()
        .filter(|b| matches!(b, file_parser::domain::ir::ParsedBlock::Heading { .. }))
        .count();

    assert!(
        heading_count >= 2,
        "Multi-sheet file should have at least 2 sheet headings, found {heading_count}"
    );
}

#[tokio::test]
async fn test_xlsx_parser_nonexistent_file() {
    let parser = SpreadsheetParser::new();
    let test_file = PathBuf::from("/nonexistent/path/to/file.xlsx");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(result.is_err(), "Should fail for non-existent file");
}

#[tokio::test]
async fn test_xlsx_parser_invalid_xlsx_bytes() {
    let parser = SpreadsheetParser::new();
    let invalid_bytes = bytes::Bytes::from_static(b"This is not a valid XLSX file content");

    let result = parser
        .parse_bytes(Some("invalid.xlsx"), None, invalid_bytes)
        .await;

    assert!(result.is_err(), "Should fail for invalid XLSX bytes");
}

#[tokio::test]
async fn test_xlsx_parser_parse_bytes() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("simple_data.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let file_content = std::fs::read(&test_file).expect("Failed to read test file");
    let bytes = bytes::Bytes::from(file_content);

    let result = parser
        .parse_bytes(Some("simple_data.xlsx"), None, bytes)
        .await;

    assert!(
        result.is_ok(),
        "Failed to parse XLSX bytes: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
}

#[tokio::test]
async fn test_xlsx_parser_parse_bytes_unrecognized_format() {
    let parser = SpreadsheetParser::new();
    // Random bytes that don't match any Excel magic bytes
    let invalid_bytes = bytes::Bytes::from_static(b"This is not a valid Excel file content");

    let result = parser
        .parse_bytes(Some("test.xlsx"), None, invalid_bytes)
        .await;

    // Should fail since the bytes don't represent a valid Excel file
    assert!(result.is_err(), "Should fail for unrecognized format");
}

#[tokio::test]
async fn test_xlsx_parser_parse_bytes_with_ole_magic_but_invalid() {
    let parser = SpreadsheetParser::new();
    // OLE magic bytes (D0 CF 11 E0) but invalid content after
    let mut invalid_bytes = vec![0xD0, 0xCF, 0x11, 0xE0];
    invalid_bytes.extend_from_slice(b"invalid content after magic");
    let invalid_bytes = bytes::Bytes::from(invalid_bytes);

    let result = parser
        .parse_bytes(Some("test.xls"), None, invalid_bytes)
        .await;

    // Should fail since the content is invalid despite matching OLE magic bytes
    assert!(result.is_err(), "Should fail for invalid XLS content");
}

#[tokio::test]
async fn test_xlsx_parser_extracts_tables() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("simple_data.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;
    let document = result.expect("Failed to parse XLSX");

    // Find table blocks
    let table_count = document
        .blocks
        .iter()
        .filter(|b| matches!(b, file_parser::domain::ir::ParsedBlock::Table(_)))
        .count();

    assert!(
        table_count >= 1,
        "XLSX should contain at least one table block, found {table_count}"
    );
}

#[tokio::test]
async fn test_xlsx_parser_merged_cells() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("merged_cells.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse XLSX with merged cells: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");

    // Find table blocks and verify structure
    let tables: Vec<_> = document
        .blocks
        .iter()
        .filter_map(|b| {
            if let file_parser::domain::ir::ParsedBlock::Table(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    assert!(!tables.is_empty(), "Should have at least one table");

    // Merged cells: the value sits in the top-left cell of the merge.
    // Verify we can parse the file without errors and get the expected row count.
    let first_table = tables[0];
    assert!(
        first_table.rows.len() >= 3,
        "Merged cells test file should have at least 3 rows, found {}",
        first_table.rows.len()
    );
}

#[tokio::test]
async fn test_xlsx_parser_formula_cells() {
    let parser = SpreadsheetParser::new();
    let test_file = get_test_file_path("formula_cells.xlsx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse XLSX with formulas: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");

    // Find table blocks
    let tables: Vec<_> = document
        .blocks
        .iter()
        .filter_map(|b| {
            if let file_parser::domain::ir::ParsedBlock::Table(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    assert!(!tables.is_empty(), "Should have at least one table");

    // The parser reads cached formula results from xlsx files.
    // Note: Files created programmatically (e.g., openpyxl) without Excel
    // may not have cached values, resulting in empty cells for formulas.
    // Files saved by Excel will have computed values cached.
    let first_table = tables[0];

    // Extract all cell text content for verification
    let cell_texts: Vec<String> = first_table
        .rows
        .iter()
        .flat_map(|row| {
            row.cells.iter().filter_map(|cell| {
                cell.blocks.first().and_then(|block| {
                    if let file_parser::domain::ir::ParsedBlock::Paragraph { inlines } = block {
                        inlines.first().and_then(|inline| {
                            if let file_parser::domain::ir::Inline::Text { text, .. } = inline {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
            })
        })
        .collect();

    // Verify we can read the regular (non-formula) cells
    assert!(
        cell_texts.iter().any(|t| t == "10"),
        "Should find value 10 in cells, found: {cell_texts:?}"
    );
    assert!(
        cell_texts.iter().any(|t| t == "20"),
        "Should find value 20 in cells, found: {cell_texts:?}"
    );

    // Formula text (starting with =) should NOT appear in output
    // Formula cells give either computed values or empty, never raw formula text
    assert!(
        !cell_texts.iter().any(|t| t.starts_with('=')),
        "Raw formula text should not appear in output, found cells: {cell_texts:?}"
    );
}

#[tokio::test]
async fn test_xlsx_parser_one_heading_per_sheet() {
    use file_parser::domain::ir::{Inline, ParsedBlock};

    let parser = SpreadsheetParser::new();
    let document = parser
        .parse_local_path(&get_test_file_path("multi_sheet.xlsx"), None)
        .await
        .expect("multi_sheet.xlsx parses");
    let headings: Vec<&str> = document
        .blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Heading { inlines, .. } => match inlines.first() {
                Some(Inline::Text { text, .. }) => Some(text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    // Each sheet name exactly once, in workbook order.
    assert_eq!(headings, ["Sales", "Inventory", "Summary"]);
}

#[tokio::test]
async fn test_xlsx_parser_merged_header_keeps_top_left_value_only() {
    use file_parser::domain::ir::{Inline, ParsedBlock};

    let parser = SpreadsheetParser::new();
    let document = parser
        .parse_local_path(&get_test_file_path("merged_cells.xlsx"), None)
        .await
        .expect("merged_cells.xlsx parses");
    let table = document
        .blocks
        .iter()
        .find_map(|b| match b {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .expect("table");
    let first_row: Vec<&str> = table.rows[0]
        .cells
        .iter()
        .map(|c| match c.blocks.first() {
            Some(ParsedBlock::Paragraph { inlines }) => match inlines.first() {
                Some(Inline::Text { text, .. }) => text.as_str(),
                _ => "",
            },
            _ => "",
        })
        .collect();
    assert_eq!(first_row, ["Merged Header Title", "", ""]);
}

/// Legacy `.xls` files written by `LibreOffice` from the xlsx fixtures: the
/// same tables must come out of the BIFF8 reader as out of the XLSX reader.
mod legacy_xls {
    use super::*;
    use file_parser::domain::ir::{Inline, ParsedBlock};

    fn xls(name: &str) -> PathBuf {
        common::fixture_path("xls", name)
    }

    fn tables(document: &file_parser::domain::ir::ParsedDocument) -> Vec<Vec<Vec<String>>> {
        document
            .blocks
            .iter()
            .filter_map(|b| match b {
                ParsedBlock::Table(t) => Some(
                    t.rows
                        .iter()
                        .map(|r| {
                            r.cells
                                .iter()
                                .map(|c| match c.blocks.first() {
                                    Some(ParsedBlock::Paragraph { inlines }) => {
                                        match inlines.first() {
                                            Some(Inline::Text { text, .. }) => text.clone(),
                                            _ => String::new(),
                                        }
                                    }
                                    _ => String::new(),
                                })
                                .collect()
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn multi_sheet_xls_matches_the_xlsx() {
        let parser = SpreadsheetParser::new();
        let from_xls = parser
            .parse_local_path(&xls("multi_sheet.xls"), None)
            .await
            .expect("multi_sheet.xls parses");
        let from_xlsx = parser
            .parse_local_path(&get_test_file_path("multi_sheet.xlsx"), None)
            .await
            .expect("multi_sheet.xlsx parses");
        assert_eq!(
            from_xls.meta.content_type.as_deref(),
            Some("application/vnd.ms-excel")
        );
        assert_eq!(tables(&from_xls), tables(&from_xlsx));
    }

    #[tokio::test]
    async fn merged_and_formula_xls_parse() {
        let parser = SpreadsheetParser::new();
        for name in ["merged_cells.xls", "formula_cells.xls"] {
            let document = parser
                .parse_local_path(&xls(name), None)
                .await
                .unwrap_or_else(|e| panic!("{name} parses: {e}"));
            assert!(!tables(&document).is_empty(), "{name} has a table");
        }
    }
}
