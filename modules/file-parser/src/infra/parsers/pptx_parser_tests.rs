use super::PptxParser;
use crate::domain::parser::FileParserBackend;

#[test]
fn test_parser_id() {
    let parser = PptxParser::new();
    assert_eq!(parser.id(), "pptx");
}

#[test]
fn test_supported_extensions() {
    let parser = PptxParser::new();
    assert_eq!(parser.supported_extensions(), &["pptx"]);
}
