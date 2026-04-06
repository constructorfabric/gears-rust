use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::plugin::{PluginError, TransformPlugin, TransformRequestContext};

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Built-in transform plugin that injects or propagates `X-Request-ID` headers.
///
/// - **on_request**: If the inbound request contains an `X-Request-ID` header,
///   propagate it unchanged. Otherwise, generate a new UUID v4 and inject it.
/// - **on_response / on_error**: Default no-op. Cross-phase state sharing
///   (propagating request ID to response) is a future enhancement.
pub struct RequestIdTransformPlugin;

#[async_trait]
impl TransformPlugin for RequestIdTransformPlugin {
    async fn on_request(&self, ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
        let has_request_id = ctx
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case(REQUEST_ID_HEADER));
        if !has_request_id {
            ctx.headers
                .push((REQUEST_ID_HEADER.to_string(), Uuid::new_v4().to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "request_id_transform_tests.rs"]
mod request_id_transform_tests;
