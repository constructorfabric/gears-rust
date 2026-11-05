#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

use file_parser::domain::error::DomainError;
use file_parser::domain::parser::FileParserBackend;
use file_parser::infra::parsers::KreuzbergParser;
use std::path::PathBuf;

mod common;

fn inline_text(inlines: &[file_parser::domain::ir::Inline]) -> String {
    use file_parser::domain::ir::Inline;
    inlines
        .iter()
        .map(|i| match i {
            Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => {
                text.as_str()
            }
        })
        .collect()
}

fn cell_text(cell: &file_parser::domain::ir::TableCell) -> String {
    use file_parser::domain::ir::ParsedBlock;
    cell.blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Paragraph { inlines } => Some(inline_text(inlines)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn get_test_file_path(filename: &str) -> PathBuf {
    common::fixture_path("docx", filename)
}

#[tokio::test]
async fn test_docx_parser_basic_info() {
    let parser = KreuzbergParser::new();

    assert_eq!(parser.id(), "kreuzberg");
    assert!(parser.supported_extensions().contains(&"docx"));
}

#[tokio::test]
async fn test_docx_parser_with_working_file() {
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_multilingual.docx");

    // Skip test if file doesn't exist (not all test files may be available)
    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse working DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");

    // Verify document metadata
    assert!(document.meta.original_filename.is_some());
    assert_eq!(
        document.meta.original_filename.as_deref(),
        Some("test_file_1table_multilingual.docx")
    );
}

#[tokio::test]
async fn test_docx_parser_parse_bytes_without_any_hint_errors() {
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_multilingual.docx");
    let bytes = bytes::Bytes::from(std::fs::read(&test_file).unwrap());

    let err = parser.parse_bytes(None, None, bytes).await.unwrap_err();

    assert!(matches!(err, DomainError::ParseError { .. }), "got {err:?}");
    assert!(
        err.to_string().contains("Cannot determine MIME type"),
        "got {err}"
    );
}

#[tokio::test]
async fn test_docx_parser_with_two_page_file() {
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_2pages_multilingual.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    assert!(
        document.blocks.iter().any(|b| matches!(
            b,
            file_parser::domain::ir::ParsedBlock::Heading { inlines, .. }
                if inline_text(inlines).contains("Fran\u{e7}ais")
        )),
        "Document should contain a heading with 'Fran\u{e7}ais'"
    );
    assert!(
        document.blocks.iter().any(|b| matches!(
            b,
            file_parser::domain::ir::ParsedBlock::Paragraph { inlines }
                if inline_text(inlines).contains("The quick brown fox jumps over the lazy dog")
        )),
        "Document should contain the English pangram paragraph"
    );
}

#[tokio::test]
async fn test_docx_parser_with_big_english_file() {
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_big_english.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    // The golden has no headings, only body paragraphs.
    assert!(
        document.blocks.iter().any(|b| matches!(
            b,
            file_parser::domain::ir::ParsedBlock::Paragraph { inlines }
                if inline_text(inlines).contains("Video provides a powerful way to help you prove your point")
        )),
        "Document should contain the 'Video provides a powerful way' paragraph"
    );
}

#[tokio::test]
async fn test_docx_parser_with_edge_cases_file_parses() {
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_edge_cases.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;
    assert!(
        result.is_ok(),
        "Failed to parse DOCX from bytes: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    assert!(document.meta.original_filename.is_some());
    assert!(
        document.blocks.iter().any(|b| matches!(
            b,
            file_parser::domain::ir::ParsedBlock::Heading { inlines, .. }
                if inline_text(inlines) == "Table with corners cases"
        )),
        "Document should contain the heading 'Table with corners cases'"
    );
}

#[tokio::test]
async fn test_docx_parser_with_image_multilingual_file() {
    // This file is 1,812,664 bytes and contains images.
    // With our fixes to docx-rust, it now parses successfully.
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_image_multilingual.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(
        result.is_ok(),
        "Failed to parse image multilingual DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    assert!(
        document.blocks.iter().all(|b| !matches!(
            b,
            file_parser::domain::ir::ParsedBlock::Image { src, .. }
                if src.as_deref().is_none_or(|s| s.trim().is_empty())
        )),
        "Images without a source should be dropped"
    );
}

#[tokio::test]
async fn test_docx_parser_parse_bytes() {
    // Test parsing from bytes instead of file path
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_multilingual.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    // Read file as bytes
    let file_bytes = std::fs::read(&test_file).expect("Failed to read test file");
    let bytes = bytes::Bytes::from(file_bytes);

    let result = parser
        .parse_bytes(
            Some("test_file_1table_multilingual.docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            bytes,
        )
        .await;

    assert!(
        result.is_ok(),
        "Failed to parse DOCX from bytes: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    assert!(document.meta.original_filename.is_some());
}

#[tokio::test]
async fn test_docx_parser_parse_bytes_without_filename() {
    // Test parsing from bytes without filename hint. Kreuzberg needs a MIME type
    // when there is no filename to infer one from (the old docx-rust parser
    // assumed DOCX), so the content type is supplied.
    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_multilingual.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let file_bytes = std::fs::read(&test_file).expect("Failed to read test file");
    let bytes = bytes::Bytes::from(file_bytes);

    let result = parser
        .parse_bytes(
            None,
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            bytes,
        )
        .await;

    assert!(
        result.is_ok(),
        "Failed to parse DOCX from bytes without filename: {:?}",
        result.err()
    );

    let document = result.unwrap();
    assert!(!document.blocks.is_empty(), "Document should have blocks");
    // KreuzbergParser falls back to the "unknown" placeholder when no filename
    // hint is provided (the old docx-rust parser left it unset)
    assert_eq!(
        document.meta.original_filename.as_deref(),
        Some("unknown"),
        "Without filename hint, original_filename should be the placeholder"
    );
}

#[tokio::test]
async fn test_docx_parser_nonexistent_file() {
    let parser = KreuzbergParser::new();
    let nonexistent = PathBuf::from("/nonexistent/path/to/file.docx");

    let result = parser.parse_local_path(&nonexistent, None).await;

    assert!(result.is_err(), "Should fail on nonexistent file");

    let error = result.err().unwrap();
    let error_msg = error.to_string();
    assert!(
        matches!(&error, DomainError::ParseError { .. })
            && error_msg.contains("IO error: File does not exist"),
        "Error should report a missing file, got: {error_msg}"
    );
}

#[tokio::test]
async fn test_docx_parser_invalid_docx_bytes() {
    let parser = KreuzbergParser::new();
    let invalid_bytes = bytes::Bytes::from("This is not a valid DOCX file");

    let result = parser
        .parse_bytes(Some("invalid.docx"), None, invalid_bytes)
        .await;

    assert!(result.is_err(), "Should fail on invalid DOCX data");

    let error = result.err().unwrap();
    let error_msg = error.to_string();
    assert!(
        matches!(&error, DomainError::ParseError { .. })
            && error_msg.contains("DOCX parsing failed"),
        "Error should mention DOCX parsing failure, got: {error_msg}"
    );
}

#[tokio::test]
async fn test_docx_parser_extracts_tables() {
    use file_parser::domain::ir::ParsedBlock;

    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_multilingual.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;
    assert!(
        result.is_ok(),
        "Failed to parse DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();

    // Count how many tables are in the document
    let table_count = document
        .blocks
        .iter()
        .filter(|block| matches!(block, ParsedBlock::Table(_)))
        .count();

    assert!(
        table_count > 0,
        "Document should contain at least one table"
    );

    // Verify table structure
    for block in &document.blocks {
        if let ParsedBlock::Table(table_block) = block {
            assert!(
                !table_block.rows.is_empty(),
                "Table should have at least one row"
            );

            for (row_idx, row) in table_block.rows.iter().enumerate() {
                assert!(
                    !row.cells.is_empty(),
                    "Row {row_idx} should have at least one cell"
                );

                for (cell_idx, cell) in row.cells.iter().enumerate() {
                    assert!(
                        !cell.blocks.is_empty(),
                        "Cell ({row_idx}, {cell_idx}) should have at least one block"
                    );
                }
            }

            eprintln!(
                "Table has {} rows with {} cells in first row",
                table_block.rows.len(),
                table_block.rows[0].cells.len()
            );
        }
    }
}

#[tokio::test]
async fn test_docx_parser_table_edge_cases() {
    use file_parser::domain::ir::ParsedBlock;

    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_1table_edge_cases.docx");

    assert!(test_file.exists(), "test fixture missing: {test_file:?}");

    let result = parser.parse_local_path(&test_file, None).await;
    assert!(
        result.is_ok(),
        "Failed to parse DOCX file: {:?}",
        result.err()
    );

    let document = result.unwrap();

    // Count tables
    let table_count = document
        .blocks
        .iter()
        .filter(|block| matches!(block, ParsedBlock::Table(_)))
        .count();

    assert!(
        table_count > 0,
        "Document should contain at least one table"
    );

    let table = document
        .blocks
        .iter()
        .find_map(|block| match block {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .unwrap();

    // Ground truth: testing/e2e/testdata/md/test_file_1table_edge_cases.md
    assert_eq!(table.rows.len(), 5, "header row plus four data rows");
    assert!(table.rows[0].is_header, "first row should be the header");
    assert!(table.rows.iter().all(|r| r.cells.len() == 4));
    assert_eq!(cell_text(&table.rows[0].cells[2]), "Pipe\\test");
    assert_eq!(
        cell_text(&table.rows[3].cells[2]),
        "Test with \u{201c}\\\u{201d} double pipe in quotes"
    );
    assert_eq!(cell_text(&table.rows[4].cells[0]), "Espa\u{f1}ol");
}

fn collect(
    blocks: &[file_parser::domain::ir::ParsedBlock],
    out: &mut Vec<(String, file_parser::domain::ir::InlineStyle)>,
) {
    for b in blocks {
        match b {
            file_parser::domain::ir::ParsedBlock::Paragraph { inlines }
            | file_parser::domain::ir::ParsedBlock::Heading { inlines, .. } => {
                for i in inlines {
                    if let file_parser::domain::ir::Inline::Text { text, style } = i {
                        out.push((text.clone(), style.clone()));
                    }
                }
            }
            file_parser::domain::ir::ParsedBlock::ListItem { blocks, .. } => collect(blocks, out),
            _ => {}
        }
    }
}

#[allow(clippy::non_ascii_literal)]
#[tokio::test]
async fn test_docx_parser_preserves_inline_styles() {
    use file_parser::domain::ir::InlineStyle;
    use file_parser::domain::markdown::MarkdownRenderer;

    let parser = KreuzbergParser::new();
    let test_file = get_test_file_path("test_file_styled_runs.docx");
    assert!(test_file.exists(), "test fixture missing: {test_file:?}");
    let doc = parser.parse_local_path(&test_file, None).await.unwrap();

    let mut segs = Vec::new();
    collect(&doc.blocks, &mut segs);
    let style_of = |t: &str| {
        segs.iter()
            .find(|(text, _)| text == t)
            .unwrap_or_else(|| panic!("no inline with text {t:?} in {segs:?}"))
            .1
            .clone()
    };

    let bold = style_of("bolded");
    assert!(bold.bold && !bold.italic && !bold.underline && !bold.strike);
    let italic = style_of("italicised");
    assert!(italic.italic && !italic.bold && !italic.underline && !italic.strike);
    let underline = style_of("underlined");
    assert!(underline.underline && !underline.bold && !underline.italic && !underline.strike);
    let strike = style_of("struck");
    assert!(strike.strike && !strike.bold && !strike.italic && !strike.underline);
    let both = style_of("bolditalic");
    assert!(both.bold && both.italic && !both.underline && !both.strike);
    // multibyte run: byte offsets must land on the right characters
    assert!(style_of("жирный").bold);
    // styled run inside a list item
    assert!(style_of("itembold").bold);
    // unstyled neighbours stay plain
    assert_eq!(style_of("plain "), InlineStyle::default());
    assert_eq!(style_of("item "), InlineStyle::default());

    let md = MarkdownRenderer::render_doc(&doc);
    for expected in [
        "**bolded**",
        "*italicised*",
        "__underlined__",
        "~~struck~~",
        "***bolditalic***",
        "**жирный**",
        "**itembold**",
    ] {
        assert!(md.contains(expected), "markdown lacks {expected:?}:\n{md}");
    }
}
