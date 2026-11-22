pub(crate) mod bounded_read;
pub mod docx_parser;
pub mod image_parser;
pub mod ir_convert;
pub mod kreuzberg_parser;
pub mod limits;
// Consumers (DOCX, PPTX, spreadsheet parsers) land in the following commits.
#[expect(dead_code, reason = "used by the OOXML parsers added in later commits")]
pub(crate) mod ooxml;
pub mod plain_text;
pub mod stub;

pub use docx_parser::DocxParser;
pub use image_parser::ImageParser;
pub use kreuzberg_parser::KreuzbergParser;
pub use limits::ParseLimits;
pub use plain_text::PlainTextParser;
pub use stub::StubParser;
