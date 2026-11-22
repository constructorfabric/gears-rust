#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

use file_parser::domain::ir::{Inline, ParsedBlock, ParsedDocument, ParsedSource};
use file_parser::domain::parser::FileParserBackend;
use file_parser::infra::parsers::DocxParser;
use std::path::PathBuf;

mod common;

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn fixture(filename: &str) -> PathBuf {
    common::fixture_path("docx", filename)
}

async fn parse_fixture(filename: &str) -> ParsedDocument {
    let path = fixture(filename);
    assert!(path.exists(), "test fixture missing: {}", path.display());
    DocxParser::new()
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

fn headings(doc: &ParsedDocument) -> Vec<(u8, String)> {
    doc.blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Heading { level, inlines } => Some((*level, text_of(inlines))),
            _ => None,
        })
        .collect()
}

fn count(doc: &ParsedDocument, pick: impl Fn(&ParsedBlock) -> bool) -> usize {
    doc.blocks.iter().filter(|b| pick(b)).count()
}

fn tables(doc: &ParsedDocument) -> Vec<&file_parser::domain::ir::TableBlock> {
    doc.blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn parser_identity() {
    let parser = DocxParser::new();
    assert_eq!(parser.id(), "docx");
    assert_eq!(parser.supported_extensions(), &["docx"]);
}

#[tokio::test]
async fn headings_come_from_style_names_and_outline_levels() {
    let doc = parse_fixture("test_file_2pages_multilingual.docx").await;
    let found = headings(&doc);
    assert_eq!(found.len(), 9, "{found:?}");
    assert!(found.contains(&(1, "Fran\u{e7}ais (French)".to_owned())));
    assert!(found.contains(&(
        3,
        "\u{420}\u{443}\u{441}\u{441}\u{43a}\u{438}\u{439} (Russian)".to_owned()
    )));
    assert!(found.contains(&(2, "English".to_owned())));
    assert_eq!(doc.meta.content_type.as_deref(), Some(DOCX_MIME));
    assert_eq!(
        doc.meta.original_filename.as_deref(),
        Some("test_file_2pages_multilingual.docx")
    );
}

#[tokio::test]
async fn preformatted_paragraphs_become_one_code_block() {
    let doc = parse_fixture("test_file_2pages_multilingual.docx").await;
    let code: Vec<&String> = doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            ParsedBlock::CodeBlock { code, .. } => Some(code),
            _ => None,
        })
        .collect();
    assert_eq!(code.len(), 1);
    assert!(code[0].contains("  - Hyphen: -"), "{}", code[0]);
    assert!(code[0].contains("Punctuation & Marks:"), "{}", code[0]);

    let pasted = parse_fixture("test_file_two_pages_international.docx").await;
    assert_eq!(
        count(&pasted, |b| matches!(b, ParsedBlock::CodeBlock { .. })),
        1
    );
    assert_eq!(
        count(&pasted, |b| matches!(b, ParsedBlock::HorizontalRule)),
        0
    );
    assert!(headings(&pasted).is_empty());
}

#[tokio::test]
async fn big_document_has_one_page_break_and_no_empty_images() {
    let doc = parse_fixture("test_file_big_english.docx").await;
    assert_eq!(count(&doc, |b| matches!(b, ParsedBlock::PageBreak)), 1);
    assert_eq!(count(&doc, |b| matches!(b, ParsedBlock::Image { .. })), 0);
    // Words split across runs join without separators.
    let text = format!("{:?}", doc.blocks);
    assert!(text.contains("Online Video"));
}

#[tokio::test]
async fn tables_have_a_header_row_and_no_empty_cells() {
    for name in [
        "test_file_1table_multilingual.docx",
        "test_file_1table_image_multilingual.docx",
        "test_file_1table_edge_cases.docx",
    ] {
        let doc = parse_fixture(name).await;
        let found = tables(&doc);
        assert_eq!(found.len(), 1, "{name}");
        let table = found[0];
        assert!(table.rows[0].is_header, "{name}");
        assert!(table.rows[1..].iter().all(|r| !r.is_header), "{name}");
        let width = table.rows[0].cells.len();
        for row in &table.rows {
            assert_eq!(row.cells.len(), width, "{name}");
            assert!(row.cells.iter().all(|c| !c.blocks.is_empty()), "{name}");
        }
    }
}

#[tokio::test]
async fn title_styles_are_level_one_headings() {
    let doc = parse_fixture("test_file_1table_multilingual.docx").await;
    assert_eq!(
        headings(&doc),
        [(1, "Multilingual Table Example".to_owned())]
    );
    let edge = parse_fixture("test_file_1table_edge_cases.docx").await;
    assert_eq!(
        headings(&edge),
        [(1, "Table with corners cases".to_owned())]
    );
    let image = parse_fixture("test_file_1table_image_multilingual.docx").await;
    // The picture has no description: no image block, and never two.
    assert_eq!(count(&image, |b| matches!(b, ParsedBlock::Image { .. })), 0);
}

#[tokio::test]
async fn parse_bytes_reports_an_uploaded_source() {
    let bytes = std::fs::read(fixture("test_file_1table_multilingual.docx")).unwrap();
    let doc = DocxParser::new()
        .parse_bytes(Some("upload.docx"), None, bytes::Bytes::from(bytes.clone()))
        .await
        .expect("parses");
    assert_eq!(
        doc.meta.source,
        ParsedSource::Uploaded {
            original_name: "upload.docx".to_owned()
        }
    );
    assert_eq!(doc.title.as_deref(), Some("upload.docx"));

    let anonymous = DocxParser::new()
        .parse_bytes(None, None, bytes::Bytes::from(bytes))
        .await
        .expect("parses");
    assert_eq!(
        anonymous.meta.source,
        ParsedSource::Uploaded {
            original_name: "unknown".to_owned()
        }
    );
}

#[tokio::test]
async fn bad_inputs_are_errors() {
    let parser = DocxParser::new();
    let missing = parser
        .parse_local_path(&PathBuf::from("/nonexistent/file.docx"), None)
        .await
        .expect_err("missing file");
    assert!(
        missing.to_string().contains("Failed to read file"),
        "{missing}"
    );
    let garbage = parser
        .parse_bytes(
            Some("x.docx"),
            None,
            bytes::Bytes::from_static(b"not a docx"),
        )
        .await
        .expect_err("garbage");
    assert!(garbage.to_string().contains("DOCX"), "{garbage}");
    let small = DocxParser::new().with_max_bytes(16);
    assert!(
        small
            .parse_local_path(&fixture("test_file_1table_multilingual.docx"), None)
            .await
            .is_err()
    );
}
