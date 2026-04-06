use async_trait::async_trait;

use crate::domain::plugin::{GuardContext, GuardDecision, GuardPlugin, PluginError};

/// Guard plugin that enforces required headers on requests and responses.
///
/// - **Request phase**: Rejects requests missing any configured required headers
///   (e.g. enforce `X-Correlation-ID`, `Accept`, or API version headers).
/// - **Response phase**: Rejects upstream responses missing required headers
///   (e.g. block responses lacking `Content-Type` — defense against compromised upstreams).
pub struct RequiredHeadersGuardPlugin;

/// Parse a comma-separated list of header names, trimming whitespace and lowercasing.
/// Empty/blank entries are skipped.
fn parse_header_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Check if all required headers are present (case-insensitive) in the given header map.
/// Returns the first missing header name, or `None` if all are present.
fn find_missing_header(required: &[String], headers: &[(String, String)]) -> Option<String> {
    for name in required {
        let found = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if !found {
            return Some(name.clone());
        }
    }
    None
}

#[async_trait]
impl GuardPlugin for RequiredHeadersGuardPlugin {
    async fn guard_request(&self, ctx: &GuardContext) -> Result<GuardDecision, PluginError> {
        let required = match ctx.config.get("required_request_headers") {
            Some(v) if !v.trim().is_empty() => parse_header_list(v),
            _ => return Ok(GuardDecision::Allow),
        };

        if let Some(missing) = find_missing_header(&required, &ctx.headers) {
            return Ok(GuardDecision::Reject {
                status: 400,
                error_code: "REQUIRED_HEADER_MISSING".into(),
                detail: format!("Missing required header: {missing}"),
            });
        }

        Ok(GuardDecision::Allow)
    }

    async fn guard_response(&self, ctx: &GuardContext) -> Result<GuardDecision, PluginError> {
        let required = match ctx.config.get("required_response_headers") {
            Some(v) if !v.trim().is_empty() => parse_header_list(v),
            _ => return Ok(GuardDecision::Allow),
        };

        if let Some(missing) = find_missing_header(&required, &ctx.headers) {
            return Ok(GuardDecision::Reject {
                status: 502,
                error_code: "REQUIRED_HEADER_MISSING".into(),
                detail: format!("Upstream response missing required header: {missing}"),
            });
        }

        Ok(GuardDecision::Allow)
    }
}

#[cfg(test)]
#[path = "required_headers_guard_tests.rs"]
mod required_headers_guard_tests;
