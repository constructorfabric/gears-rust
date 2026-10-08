//! The one small interface through which Construct calls language models.
//!
//! The planner and the sensitive-data checks depend on this trait only. Which
//! model service answers is deployment configuration, so the rest of Construct
//! never knows whether the call goes through OAGW to an OpenAI-compatible
//! server or to the LLM gateway (`cpt-cf-construct-adr-one-model-interface`).

use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// One message of the conversation sent to the model.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The instructions.
    System(String),
    /// Input the model reasons over, such as a record's payload and the
    /// numbered profile.
    User(String),
    /// An earlier answer of the model, with the tool calls it made.
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// What a tool returned for one tool call.
    ToolResult { call_id: String, content: String },
}

/// A tool the model may call.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// The JSON schema of the tool's arguments.
    pub parameters: Value,
}

/// A tool call the model made.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments, parsed from the model's JSON.
    pub arguments: Value,
}

/// What kind of answer the caller wants.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerKind {
    /// Free text, or tool calls when tools are given.
    Text,
    /// One JSON value that fits `schema`, for example a check's verdict.
    Structured { name: String, schema: Value },
}

/// One call to the model.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub answer: AnswerKind,
    /// The most tokens the answer may use. `None` leaves it to the model
    /// service.
    pub max_output_tokens: Option<u32>,
}

/// The model's answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ModelOutput {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Structured(Value),
}

/// Tokens one call used, for the planner's token cap.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The model's answer and what it cost.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub output: ModelOutput,
    pub usage: Usage,
}

/// Why a call to the model failed. Every variant drops the record that needed
/// the call; none of them carries the prompt or the answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// No answer within the configured time.
    #[error("the model gave no answer in time")]
    Timeout,
    /// The model service is not reachable now, or asked the caller to retry.
    #[error("the model service is unavailable: {0}")]
    Unavailable(String),
    /// The model service declined the request, for example a content filter or
    /// a request it does not accept.
    #[error("the model service refused the request: {0}")]
    Refused(String),
    /// The answer could not be read as the kind the caller asked for.
    #[error("the model's answer could not be read: {0}")]
    BadAnswer(String),
}

/// A language model behind one small interface.
///
/// @cpt-dod:cpt-cf-construct-dod-model-client-interface:p1
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Send one request and wait for the answer.
    ///
    /// # Errors
    ///
    /// [`ModelError`] when the call fails or the answer cannot be read.
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: ModelRequest,
    ) -> Result<ModelResponse, ModelError>;
}
