//! file-parser SDK
//!
//! Transport-agnostic public API for consumers of the `file-parser` gear:
//! - [`FileParserClientV1`] trait for inter-gear communication (resolve via
//!   `ClientHub::get::<dyn FileParserClientV1>()`)
//! - Model types ([`ParseBytesRequest`], [`ParsedText`])
//! - Error type ([`FileParserClientError`])

#![forbid(unsafe_code)]

pub mod client;
pub mod errors;
pub mod models;

pub use client::FileParserClientV1;
pub use errors::FileParserClientError;
pub use models::{Detection, ParseBytesRequest, ParsedText};
