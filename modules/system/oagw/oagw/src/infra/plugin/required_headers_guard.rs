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
mod tests {
    use std::collections::HashMap;

    use modkit_security::SecurityContext;
    use uuid::Uuid;

    use super::*;
    use crate::domain::plugin::GuardPlugin;

    fn test_security_context() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    fn make_request_ctx(
        config: HashMap<String, String>,
        headers: Vec<(String, String)>,
    ) -> GuardContext {
        GuardContext {
            method: "POST".to_string(),
            path: "/v1/test".to_string(),
            status: None,
            headers,
            config,
            security_context: test_security_context(),
        }
    }

    fn make_response_ctx(
        config: HashMap<String, String>,
        headers: Vec<(String, String)>,
    ) -> GuardContext {
        GuardContext {
            method: "POST".to_string(),
            path: "/v1/test".to_string(),
            status: Some(200),
            headers,
            config,
            security_context: test_security_context(),
        }
    }

    // -- Unconfigured --

    #[tokio::test]
    async fn allows_when_not_configured() {
        let plugin = RequiredHeadersGuardPlugin;
        let ctx = make_request_ctx(HashMap::new(), vec![]);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    // -- Request phase --

    #[tokio::test]
    async fn allows_request_with_required_headers() {
        let plugin = RequiredHeadersGuardPlugin;
        let config = HashMap::from([(
            "required_request_headers".into(),
            "x-correlation-id, accept".into(),
        )]);
        let headers = vec![
            ("x-correlation-id".into(), "abc-123".into()),
            ("accept".into(), "application/json".into()),
        ];
        let ctx = make_request_ctx(config, headers);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    #[tokio::test]
    async fn rejects_request_missing_header() {
        let plugin = RequiredHeadersGuardPlugin;
        let config =
            HashMap::from([("required_request_headers".into(), "x-correlation-id".into())]);
        let ctx = make_request_ctx(config, vec![]);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert!(matches!(
            decision,
            GuardDecision::Reject { status: 400, .. }
        ));
    }

    #[tokio::test]
    async fn request_check_is_case_insensitive() {
        let plugin = RequiredHeadersGuardPlugin;
        let config =
            HashMap::from([("required_request_headers".into(), "x-correlation-id".into())]);
        // Key uses different casing than the config.
        let headers = vec![("X-Correlation-ID".into(), "abc-123".into())];
        let ctx = make_request_ctx(config, headers);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    // -- Response phase --

    #[tokio::test]
    async fn allows_response_with_required_headers() {
        let plugin = RequiredHeadersGuardPlugin;
        let config = HashMap::from([("required_response_headers".into(), "content-type".into())]);
        let headers = vec![("content-type".into(), "application/json".into())];
        let ctx = make_response_ctx(config, headers);
        let decision = plugin.guard_response(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    #[tokio::test]
    async fn rejects_response_missing_header() {
        let plugin = RequiredHeadersGuardPlugin;
        let config = HashMap::from([("required_response_headers".into(), "content-type".into())]);
        let ctx = make_response_ctx(config, vec![]);
        let decision = plugin.guard_response(&ctx).await.unwrap();
        assert!(matches!(
            decision,
            GuardDecision::Reject { status: 502, .. }
        ));
    }

    // -- Multiple headers --

    #[tokio::test]
    async fn multiple_required_headers_first_missing_reported() {
        let plugin = RequiredHeadersGuardPlugin;
        let config = HashMap::from([(
            "required_request_headers".into(),
            "x-correlation-id, x-api-version".into(),
        )]);
        let ctx = make_request_ctx(config, vec![]);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        match decision {
            GuardDecision::Reject { detail, .. } => {
                assert!(
                    detail.contains("x-correlation-id"),
                    "should report first missing header, got: {detail}"
                );
            }
            _ => panic!("expected Reject"),
        }
    }

    // -- Phase isolation --

    #[tokio::test]
    async fn skips_request_check_in_response_phase() {
        let plugin = RequiredHeadersGuardPlugin;
        // Only required_request_headers configured — response phase should allow.
        let config =
            HashMap::from([("required_request_headers".into(), "x-correlation-id".into())]);
        let ctx = make_response_ctx(config, vec![]);
        let decision = plugin.guard_response(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    #[tokio::test]
    async fn skips_response_check_in_request_phase() {
        let plugin = RequiredHeadersGuardPlugin;
        // Only required_response_headers configured — request phase should allow.
        let config = HashMap::from([("required_response_headers".into(), "content-type".into())]);
        let ctx = make_request_ctx(config, vec![]);
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }

    // -- Edge cases --

    #[tokio::test]
    async fn blank_header_names_are_ignored() {
        let plugin = RequiredHeadersGuardPlugin;
        let config = HashMap::from([("required_request_headers".into(), ", , ,".into())]);
        let ctx = make_request_ctx(config, vec![]);
        // All entries are blank after trim — effectively unconfigured.
        let decision = plugin.guard_request(&ctx).await.unwrap();
        assert_eq!(decision, GuardDecision::Allow);
    }
}
