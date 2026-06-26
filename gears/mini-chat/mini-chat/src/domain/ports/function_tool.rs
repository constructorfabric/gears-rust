//! Domain port for pluggable LLM function tools.
//!
//! The [`FunctionTool`] trait decouples the agentic provider loop from any
//! specific custom-tool implementation. Each tool advertises its provider-
//! agnostic [`LlmFunctionDef`] descriptor and executes a single tool call,
//! returning the text injected back as a `function_call_output`.
//!
//! The agentic loop in `stream_service/provider_task.rs` dispatches incoming
//! `function_call`s by name through a registry of these trait objects, so a
//! new tool (e.g. web search via Exa) is added by implementing this trait and
//! registering it — no new branch in the loop.
//!
//! Concrete implementations live in `infra::llm::providers` (e.g.
//! `exa_search_tool.rs`).

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::llm::LlmFunctionDef;

/// Errors from a function-tool invocation.
///
/// The agentic loop never fails the turn on these — it injects a textual
/// fallback as the `function_call_output` so the model can keep going
/// (same graceful-degradation policy as `search_knowledge`).
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum FunctionToolError {
    /// Upstream explicitly rejected the request (4xx) or the arguments were invalid.
    #[error("function tool rejected: {0}")]
    Rejected(String),
    /// Upstream unavailable or transient failure (5xx, timeout).
    #[error("function tool unavailable: {0}")]
    Unavailable(String),
    /// Configuration error (missing alias, bad credentials, etc.).
    #[error("function tool configuration error: {0}")]
    Configuration(String),
}

/// Port for a pluggable LLM function tool.
///
/// Implementations are registered in a `HashMap<String, Arc<dyn FunctionTool>>`
/// keyed by [`FunctionTool::name`]. Per-model enablement is resolved upstream;
/// the loop only ever sees the tools enabled for the current request.
#[async_trait]
pub trait FunctionTool: Send + Sync {
    /// Tool name the model calls and the loop dispatches on. MUST equal the
    /// `name` of the [`LlmFunctionDef`] returned by [`Self::definition`].
    fn name(&self) -> &str;

    /// Provider-agnostic function descriptor (name, description, JSON-schema
    /// params) advertised to the model.
    fn definition(&self) -> LlmFunctionDef;

    /// Guard instruction appended to the system prompt when this tool is
    /// enabled (steers the model on when/how to call it). `None` = no guard.
    fn system_prompt_guard(&self) -> Option<String> {
        None
    }

    /// Maximum calls allowed per message before the loop injects a soft
    /// limit notice instead of executing (graceful degradation).
    fn max_calls(&self) -> u32;

    /// Execute a single tool call.
    ///
    /// `input` is the parsed arguments JSON the model produced. The returned
    /// string is injected verbatim as the `function_call_output` text.
    async fn execute(
        &self,
        ctx: SecurityContext,
        input: serde_json::Value,
    ) -> Result<String, FunctionToolError>;
}
