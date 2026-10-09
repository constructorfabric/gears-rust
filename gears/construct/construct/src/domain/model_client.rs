use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// One message of the conversation sent to a model.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The instructions.
    System(String),
    /// The input the model works on.
    User(String),
    /// An earlier answer of the model, with the tool calls it made.
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// What a tool returned for one tool call.
    ToolResult { call_id: String, content: String },
}

/// A tool the model may call: its name, what it does and the JSON schema of its arguments.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// A tool call the model made, with its arguments parsed from JSON.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// The kind of answer a caller wants: free text or tool calls, or one JSON value that fits a schema.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerKind {
    Text,
    Structured { name: String, schema: Value },
}

/// One call to a model.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub answer: AnswerKind,
    /// The most tokens the answer may use; `None` leaves it to the model service.
    pub max_output_tokens: Option<u32>,
}

/// What a model answered: text, tool calls, or a JSON value for a structured answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ModelOutput {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Structured(Value),
}

/// The tokens one call used.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A model's answer, with the tokens it used when the model service reported them.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub output: ModelOutput,
    /// `None` when the model service did not report usage; unknown is not zero.
    pub usage: Option<Usage>,
}

/// Why a call to a model failed. No variant carries the prompt, the answer or a model service's message.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("the model gave no answer in time")]
    Timeout,
    #[error("the model service is unavailable: {0}")]
    Unavailable(String),
    #[error("the model service refused the request: {0}")]
    Refused(String),
    #[error("the model's answer could not be read: {0}")]
    BadAnswer(String),
}

/// A language model behind one small interface.
///
/// @cpt-dod:cpt-cf-construct-dod-model-client-interface:p1
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Sends one request and waits for the whole answer.
    ///
    /// # Errors
    ///
    /// [`ModelError`] when the call fails or the answer cannot be read as the kind asked for.
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError>;
}
