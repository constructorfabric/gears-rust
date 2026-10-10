use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        call_id: String,
        content: String,
    },
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerKind {
    Text,
    Structured { name: String, schema: Value },
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub answer: AnswerKind,
    pub max_output_tokens: Option<u32>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ModelOutput {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Structured(Value),
}

#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub output: ModelOutput,
    pub usage: Usage,
}

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

/// @cpt-dod:cpt-cf-construct-dod-model-client-interface:p1
#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: ModelRequest,
    ) -> Result<ModelResponse, ModelError>;
}
