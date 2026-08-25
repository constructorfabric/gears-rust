use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::errors::FileParserClientError;
use crate::models::{ParseBytesRequest, ParsedText};

/// Public API for the `file-parser` gear, consumed by other gears via
/// `ClientHub::get::<dyn FileParserClientV1>()`.
///
/// The in-process implementation (`file_parser::domain::local_client::FileParserLocalClient`)
/// is registered in `ClientHub` by `FileParserGear::init` — see
/// `gears/file-parser/file-parser/src/gear.rs`.
#[async_trait]
pub trait FileParserClientV1: Send + Sync {
    /// Parse raw bytes and return their Markdown-rendered text content.
    async fn parse_bytes(
        &self,
        ctx: &SecurityContext,
        req: ParseBytesRequest,
    ) -> Result<ParsedText, FileParserClientError>;
}
