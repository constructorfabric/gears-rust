#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

use file_parser::domain::ir::{Inline, ParsedBlock, ParsedDocument, ParsedSource};
use file_parser::domain::parser::FileParserBackend;
use file_parser::infra::parsers::PdfParser;
use std::path::PathBuf;

mod common;

fn fixture(filename: &str) -> PathBuf {
    common::fixture_path("pdf", filename)
}

async fn parse_fixture(filename: &str) -> ParsedDocument {
    let path = fixture(filename);
    assert!(path.exists(), "test fixture missing: {}", path.display());
    PdfParser::new()
        .parse_local_path(&path, None)
        .await
        .unwrap_or_else(|e| panic!("failed to parse {filename}: {e}"))
}

fn text_of(block: &ParsedBlock) -> Option<String> {
    match block {
        ParsedBlock::Paragraph { inlines } | ParsedBlock::Heading { inlines, .. } => Some(
            inlines
                .iter()
                .map(|i| match i {
                    Inline::Text { text, .. }
                    | Inline::Link { text, .. }
                    | Inline::Code { text, .. } => text.as_str(),
                })
                .collect(),
        ),
        // List items read as Markdown would show them.
        ParsedBlock::ListItem { blocks, .. } => Some(format!(
            "- {}",
            blocks
                .iter()
                .filter_map(text_of)
                .collect::<Vec<_>>()
                .join(" ")
        )),
        _ => None,
    }
}

fn all_lines(doc: &ParsedDocument) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(text_of)
        .flat_map(|t| t.lines().map(str::to_owned).collect::<Vec<_>>())
        .collect()
}

fn page_breaks(doc: &ParsedDocument) -> usize {
    doc.blocks
        .iter()
        .filter(|b| matches!(b, ParsedBlock::PageBreak))
        .count()
}

#[tokio::test]
async fn parser_identity() {
    let parser = PdfParser::new();
    assert_eq!(parser.id(), "pdf");
    assert_eq!(parser.supported_extensions(), ["pdf"]);
}

#[tokio::test]
async fn one_page_document() {
    let doc = parse_fixture("test_file_one_page_en.pdf").await;
    assert_eq!(all_lines(&doc), ["test pdf file"]);
    assert_eq!(page_breaks(&doc), 0);
    assert_eq!(doc.meta.content_type.as_deref(), Some("application/pdf"));
    assert_eq!(doc.title.as_deref(), Some("test_file_one_page_en.pdf"));
    assert_eq!(
        doc.meta.original_filename.as_deref(),
        Some("test_file_one_page_en.pdf")
    );
}

#[tokio::test]
async fn accented_latin_text() {
    let doc = parse_fixture("test_file_one_page_international.pdf").await;
    assert_eq!(
        all_lines(&doc),
        [
            "This is a fa\u{e7}ade file \u{2014} d\u{e9}j\u{e0} vu with na\u{ef}ve co\u{f6}peration and touch of jalape\u{f1}o."
        ]
    );
}

#[tokio::test]
async fn empty_pages_keep_their_page_breaks() {
    let doc = parse_fixture("test_file_three_pages_two_empty_en.pdf").await;
    assert!(matches!(doc.blocks.first(), Some(ParsedBlock::PageBreak)));
    assert_eq!(page_breaks(&doc), 2);
    assert_eq!(
        all_lines(&doc),
        ["test pdf file with 3 pages total and 2 empty pages"]
    );
}

#[tokio::test]
async fn multilingual_lines_in_logical_order() {
    let doc = parse_fixture("test_file_two_pages_international.pdf").await;
    assert_eq!(page_breaks(&doc), 1);
    let items = doc
        .blocks
        .iter()
        .filter(|b| matches!(b, ParsedBlock::ListItem { ordered: false, .. }))
        .count();
    assert_eq!(items, 18, "the symbol lines are list items");
    let lines = all_lines(&doc);
    for expected in [
        "Multilingual UTF-8 Characters Test",
        "Russian: \u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}. \u{41a}\u{430}\u{43a} \u{434}\u{435}\u{43b}\u{430}?",
        "Arabic: \u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}",
        "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}",
        "Chinese (Simplified): \u{4f60}\u{597d} \u{4e16}\u{754c}",
        "- \u{201c}Double quotes\u{201d}",
        "- Arrow: \u{2192}",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "missing line {expected:?} in {lines:#?}"
        );
    }
}

#[tokio::test]
async fn parse_bytes_reports_an_uploaded_source() {
    let bytes = std::fs::read(fixture("test_file_one_page_en.pdf")).unwrap();
    let doc = PdfParser::new()
        .parse_bytes(Some("upload.pdf"), None, bytes::Bytes::from(bytes))
        .await
        .expect("parses");
    assert_eq!(
        doc.meta.source,
        ParsedSource::Uploaded {
            original_name: "upload.pdf".to_owned()
        }
    );
    assert_eq!(all_lines(&doc), ["test pdf file"]);
}

#[tokio::test]
async fn bad_inputs_are_errors() {
    let parser = PdfParser::new();
    assert!(
        parser
            .parse_local_path(&PathBuf::from("/nonexistent/file.pdf"), None)
            .await
            .is_err()
    );
    let err = parser
        .parse_bytes(Some("x.pdf"), None, bytes::Bytes::from_static(b"not a pdf"))
        .await
        .expect_err("garbage is rejected");
    assert!(err.to_string().contains("PDF"), "{err}");
    let small = PdfParser::new().with_max_bytes(16);
    assert!(
        small
            .parse_local_path(&fixture("test_file_one_page_en.pdf"), None)
            .await
            .is_err()
    );
}
