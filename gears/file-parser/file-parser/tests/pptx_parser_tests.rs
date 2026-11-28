#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

use file_parser::domain::ir::{Inline, ParsedBlock, ParsedDocument, ParsedSource};
use file_parser::domain::parser::FileParserBackend;
use file_parser::infra::parsers::PptxParser;
use std::path::PathBuf;

mod common;

const PPTX_MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn get_test_file_path(filename: &str) -> PathBuf {
    common::fixture_path("pptx", filename)
}

async fn parse_fixture(filename: &str) -> ParsedDocument {
    let path = get_test_file_path(filename);
    assert!(path.exists(), "test fixture missing: {}", path.display());
    PptxParser::new()
        .parse_local_path(&path, None)
        .await
        .unwrap_or_else(|e| panic!("failed to parse {filename}: {e}"))
}

fn text_of(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|i| match i {
            Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => {
                text.as_str()
            }
        })
        .collect()
}

fn block_text(block: &ParsedBlock) -> String {
    match block {
        ParsedBlock::Heading { inlines, .. } | ParsedBlock::Paragraph { inlines } => {
            text_of(inlines)
        }
        ParsedBlock::ListItem { blocks, .. } | ParsedBlock::Quote { blocks } => {
            blocks.iter().map(block_text).collect::<Vec<_>>().join(" ")
        }
        _ => String::new(),
    }
}

fn headings(doc: &ParsedDocument) -> Vec<String> {
    doc.blocks
        .iter()
        .filter(|b| matches!(b, ParsedBlock::Heading { level: 2, .. }))
        .map(block_text)
        .collect()
}

#[tokio::test]
async fn test_pptx_parser_basic_info() {
    let parser = PptxParser::new();
    assert_eq!(parser.id(), "pptx");
    assert_eq!(parser.supported_extensions(), ["pptx"]);
}

#[tokio::test]
async fn test_pptx_parser_with_simple_file() {
    let document = parse_fixture("simple_presentation.pptx").await;

    assert_eq!(
        document.meta.original_filename.as_deref(),
        Some("simple_presentation.pptx")
    );
    assert_eq!(document.meta.content_type.as_deref(), Some(PPTX_MIME));
    assert_eq!(headings(&document), ["Welcome to the Presentation"]);
    assert!(
        document
            .blocks
            .iter()
            .any(|b| matches!(b, ParsedBlock::Paragraph { .. })
                && block_text(b) == "This is a simple test presentation for file parser testing."),
        "body text missing: {:?}",
        document.blocks
    );
}

#[tokio::test]
async fn test_pptx_parser_with_multislide_file() {
    let document = parse_fixture("multi_slide.pptx").await;

    assert_eq!(
        headings(&document),
        [
            "Multi-Slide Presentation",
            "Slide 2: Content",
            "Slide 3: More Content"
        ]
    );
    let page_breaks = document
        .blocks
        .iter()
        .filter(|b| matches!(b, ParsedBlock::PageBreak))
        .count();
    assert_eq!(page_breaks, 2, "one page break between consecutive slides");
    assert!(
        !matches!(document.blocks.last(), Some(ParsedBlock::PageBreak)),
        "no page break after the last slide"
    );
}

#[tokio::test]
async fn test_pptx_parser_nonexistent_file() {
    let parser = PptxParser::new();
    let test_file = PathBuf::from("/nonexistent/path/to/file.pptx");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(result.is_err(), "Should fail for non-existent file");
}

#[tokio::test]
async fn test_pptx_parser_invalid_pptx_bytes() {
    let parser = PptxParser::new();
    let invalid_bytes = bytes::Bytes::from_static(b"This is not a valid PPTX file content");

    let err = parser
        .parse_bytes(Some("invalid.pptx"), None, invalid_bytes)
        .await
        .expect_err("Should fail for invalid PPTX bytes");

    assert!(err.to_string().contains("PPTX"), "{err}");
}

#[tokio::test]
async fn test_pptx_parser_parse_bytes() {
    let test_file = get_test_file_path("simple_presentation.pptx");
    let file_content = std::fs::read(&test_file).expect("Failed to read test file");

    let document = PptxParser::new()
        .parse_bytes(
            Some("simple_presentation.pptx"),
            None,
            bytes::Bytes::from(file_content),
        )
        .await
        .expect("Failed to parse PPTX bytes");

    assert_eq!(
        document.meta.source,
        ParsedSource::Uploaded {
            original_name: "simple_presentation.pptx".to_owned()
        }
    );
    assert_eq!(headings(&document), ["Welcome to the Presentation"]);
}

#[tokio::test]
async fn test_pptx_parser_respects_max_bytes() {
    let parser = PptxParser::new().with_max_bytes(16);
    let test_file = get_test_file_path("simple_presentation.pptx");

    let result = parser.parse_local_path(&test_file, None).await;

    assert!(result.is_err(), "a file over max_bytes must be rejected");
}

#[tokio::test]
async fn test_pptx_parser_with_tables() {
    let document = parse_fixture("presentation_with_table.pptx").await;

    let tables: Vec<_> = document
        .blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(tables.len(), 1, "blocks: {:?}", document.blocks);
    let table = tables[0];
    let cells: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.blocks.iter().map(block_text).collect())
                .collect()
        })
        .collect();
    assert_eq!(
        cells,
        [
            ["Name", "Role", "Department"],
            ["Alice", "Engineer", "Engineering"],
            ["Bob", "Manager", "Operations"],
        ]
    );
    assert!(table.rows[0].is_header);
    assert!(table.rows[1..].iter().all(|r| !r.is_header));
    // The table slide's title placeholder is empty.
    assert_eq!(headings(&document), ["Slide 1"]);
}

#[tokio::test]
async fn test_pptx_parser_with_lists() {
    let document = parse_fixture("presentation_with_list.pptx").await;

    assert_eq!(headings(&document), ["Key Points"]);
    // The content placeholder's bullets come from the slide master.
    let items: Vec<(u8, bool, String)> = document
        .blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::ListItem { level, ordered, .. } => Some((*level, *ordered, block_text(b))),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        [
            (0, false, "First important point".to_owned()),
            (0, false, "Second important point".to_owned()),
            (0, false, "Third important point".to_owned()),
            (1, false, "Sub-point under third".to_owned()),
        ]
    );
}
