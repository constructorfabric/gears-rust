// ToolKit needs access to the gear struct for instantiation
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === PUBLIC API (from SDK) ===
// Other gears should prefer depending on `cf-gears-file-parser-sdk` directly
// rather than this implementation crate (it pulls in kreuzberg/docx-rust
// transitively); these re-exports exist for convenience/back-compat only.
pub use file_parser_sdk::{
    Detection, FileParserClientError, FileParserClientV1, ParseBytesRequest, ParsedText,
};

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::FileParserGear;

// === INTERNAL MODULES ===
// WARNING: These gears are internal implementation details!
// They are exposed only for comprehensive testing and should NOT be used by external consumers.
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
