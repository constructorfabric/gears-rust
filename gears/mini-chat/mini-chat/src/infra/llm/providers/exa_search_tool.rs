//! Exa web-search function tool (exa.ai `/search`).
//!
//! Implements [`FunctionTool`] as `exa_search`: a generic web-search tool the
//! model can call when it needs current information. Egress goes through OAGW
//! (the apikey auth plugin injects `x-api-key`), reusing [`RagHttpClient`] as a
//! generic "JSON POST through OAGW" primitive.
//!
//! Request:  `POST /{alias}/search`
//!   `{ "query", "type", "numResults", "contents": { "highlights": true } }`
//! Response: `{ "results": [ { "title", "url", "highlights": [..] } ] }`
//!
//! Reference: <https://docs.exa.ai/reference/search-api-guide-for-coding-agents>

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use tracing::warn;

use crate::domain::llm::LlmFunctionDef;
use crate::domain::ports::{FileStorageError, FunctionTool, FunctionToolError};
use crate::infra::llm::providers::rag_http_client::RagHttpClient;

/// `exa.ai` `/search` response envelope (subset we consume).
#[derive(serde::Deserialize)]
struct ExaSearchResponse {
    #[serde(default)]
    results: Vec<ExaResult>,
}

#[derive(serde::Deserialize)]
struct ExaResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    highlights: Vec<String>,
}

/// Web search via exa.ai, dispatched by the agentic loop as `exa_search`.
pub struct ExaSearchTool {
    client: Arc<RagHttpClient>,
    /// Pre-resolved OAGW upstream alias for exa.
    upstream_alias: String,
    search_type: String,
    num_results: u32,
    max_calls: u32,
    /// Maximum characters kept per formatted result (bounds context cost).
    max_chars: usize,
    guard: String,
}

impl ExaSearchTool {
    #[must_use]
    pub fn new(
        client: Arc<RagHttpClient>,
        upstream_alias: String,
        search_type: String,
        num_results: u32,
        max_calls: u32,
        max_chars: usize,
        guard: String,
    ) -> Self {
        Self {
            client,
            upstream_alias,
            search_type,
            num_results,
            max_calls,
            max_chars,
            guard,
        }
    }

    /// Format the exa results into a compact text block for `function_call_output`.
    fn format_results(&self, resp: &ExaSearchResponse) -> String {
        use std::fmt::Write as _;

        if resp.results.is_empty() {
            return "No web results found.".to_owned();
        }
        let mut out = String::new();
        for (i, r) in resp.results.iter().enumerate() {
            let title = r.title.as_deref().unwrap_or("(untitled)");
            let url = r.url.as_deref().unwrap_or("");
            write!(out, "[{}] {title}\n{url}\n", i + 1).ok();
            if !r.highlights.is_empty() {
                out.push_str(&r.highlights.join(" ... "));
                out.push('\n');
            }
            out.push('\n');
            if out.len() >= self.max_chars {
                break;
            }
        }
        out.truncate(self.max_chars);
        out
    }
}

fn map_err(e: FileStorageError) -> FunctionToolError {
    match e {
        FileStorageError::Rejected { message, .. } => FunctionToolError::Rejected(message),
        FileStorageError::Unavailable { message } => FunctionToolError::Unavailable(message),
        other => FunctionToolError::Configuration(other.to_string()),
    }
}

#[async_trait]
impl FunctionTool for ExaSearchTool {
    // Trait signature ties the return to `&self`; the literal is `'static` but
    // the impl must match the trait, so the suggested `&'static str` is moot.
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "exa_search"
    }

    fn definition(&self) -> LlmFunctionDef {
        LlmFunctionDef {
            name: "exa_search".to_owned(),
            description: "Search the public web for current information. Use this when the \
                          answer depends on recent events or facts that may not be in your \
                          training data."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A focused web search query."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn system_prompt_guard(&self) -> Option<String> {
        if self.guard.is_empty() {
            None
        } else {
            Some(self.guard.clone())
        }
    }

    fn max_calls(&self) -> u32 {
        self.max_calls
    }

    async fn execute(
        &self,
        ctx: SecurityContext,
        input: serde_json::Value,
    ) -> Result<String, FunctionToolError> {
        let query = input.get("query").and_then(|v| v.as_str()).unwrap_or_default();
        if query.is_empty() {
            return Err(FunctionToolError::Rejected(
                "exa_search called without a 'query'".to_owned(),
            ));
        }

        let uri = format!("/{}/search", self.upstream_alias);
        let body = serde_json::json!({
            "query": query,
            "type": self.search_type,
            "numResults": self.num_results,
            "contents": { "highlights": true },
        });

        let resp: ExaSearchResponse =
            self.client.json_post(ctx, &uri, &body).await.map_err(|e| {
                warn!(error = %e, "exa_search request failed");
                map_err(e)
            })?;

        Ok(self.format_results(&resp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ExaSearchTool {
        ExaSearchTool::new(
            Arc::new(RagHttpClient::new(Arc::new(NoopOagw))),
            "api.exa.ai".to_owned(),
            "auto".to_owned(),
            10,
            3,
            2000,
            "guard text".to_owned(),
        )
    }

    // Minimal OAGW stub (never called — format_results is pure).
    struct NoopOagw;
    #[async_trait]
    impl oagw_sdk::ServiceGatewayClientV1 for NoopOagw {
        async fn create_upstream(
            &self,
            _: SecurityContext,
            _: oagw_sdk::CreateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: SecurityContext,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Upstream>, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: SecurityContext,
            _: oagw_sdk::CreateRouteRequest,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: SecurityContext,
            _: Option<uuid::Uuid>,
            _: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Route>, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
            _: oagw_sdk::UpdateRouteRequest,
        ) -> Result<oagw_sdk::Route, toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _: SecurityContext,
            _: uuid::Uuid,
        ) -> Result<(), toolkit_canonical_errors::CanonicalError> {
            unimplemented!()
        }
        async fn resolve_proxy_target(
            &self,
            _: SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), toolkit_canonical_errors::CanonicalError>
        {
            unimplemented!()
        }
        async fn proxy_request(
            &self,
            _: SecurityContext,
            _: http::Request<oagw_sdk::Body>,
        ) -> Result<http::Response<oagw_sdk::Body>, toolkit_canonical_errors::CanonicalError>
        {
            unimplemented!()
        }
    }

    #[test]
    fn definition_is_exa_search() {
        let d = tool().definition();
        assert_eq!(d.name, "exa_search");
        assert_eq!(tool().name(), "exa_search");
    }

    #[test]
    fn guard_exposed_from_config() {
        assert_eq!(tool().system_prompt_guard().as_deref(), Some("guard text"));
    }

    #[test]
    fn format_empty_results() {
        let resp = ExaSearchResponse { results: vec![] };
        assert_eq!(tool().format_results(&resp), "No web results found.");
    }

    #[test]
    fn format_results_includes_title_url_highlights() {
        let resp = ExaSearchResponse {
            results: vec![ExaResult {
                title: Some("Rust 2.0 released".to_owned()),
                url: Some("https://example.com/rust".to_owned()),
                highlights: vec!["A big release".to_owned()],
            }],
        };
        let out = tool().format_results(&resp);
        assert!(out.contains("Rust 2.0 released"));
        assert!(out.contains("https://example.com/rust"));
        assert!(out.contains("A big release"));
    }

    #[test]
    fn format_results_truncates_to_max_chars() {
        let big = "x".repeat(5000);
        let resp = ExaSearchResponse {
            results: vec![ExaResult {
                title: Some(big),
                url: Some("https://e.com".to_owned()),
                highlights: vec![],
            }],
        };
        assert!(tool().format_results(&resp).len() <= 2000);
    }
}
