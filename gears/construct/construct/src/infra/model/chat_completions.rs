use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{Method, StatusCode};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::Deserialize;
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;

use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse,
    ToolCall, ToolSpec, Usage,
};

/// @cpt-dod:cpt-cf-construct-dod-model-client-chat-completions:p1
pub struct ChatCompletionsModel {
    hub: Arc<ClientHub>,
    upstream_alias: String,
    model: String,
    timeout: Duration,
}

impl ChatCompletionsModel {
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        upstream_alias: String,
        model: String,
        timeout: Duration,
    ) -> Self {
        Self {
            hub,
            upstream_alias,
            model,
            timeout,
        }
    }
}

#[async_trait]
impl ModelClient for ChatCompletionsModel {
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        let Some(gateway) = self.hub.try_get::<dyn ServiceGatewayClientV1>() else {
            return Err(ModelError::Unavailable(
                "no OAGW client in ClientHub".to_owned(),
            ));
        };
        let body = serde_json::to_vec(&request_body(&self.model, &request)).map_err(|error| {
            ModelError::BadAnswer(format!("the request could not be encoded: {error}"))
        })?;
        let http_request = http::Request::builder()
            .method(Method::POST)
            .uri(format!("/{}", self.upstream_alias))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(Body::Bytes(Bytes::from(body)))
            .map_err(|error| {
                ModelError::Unavailable(format!("the request could not be built: {error}"))
            })?;

        let call = async {
            let response = gateway
                .proxy_request(ctx.clone(), http_request)
                .await
                .map_err(|error| ModelError::Unavailable(error.to_string()))?;
            let (parts, body) = response.into_parts();
            let bytes = body.into_bytes().await.map_err(|error| {
                ModelError::Unavailable(format!("the answer could not be read: {error}"))
            })?;
            Ok::<_, ModelError>((parts.status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(self.timeout, call)
            .await
            .map_err(|_| ModelError::Timeout)??;

        if !status.is_success() {
            return Err(status_error(status));
        }
        read_answer(&bytes, &request.answer)
    }
}

fn status_error(status: StatusCode) -> ModelError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        ModelError::Unavailable(format!("HTTP {status}"))
    } else {
        ModelError::Refused(format!("HTTP {status}"))
    }
}

fn request_body(model: &str, request: &ModelRequest) -> Value {
    let mut body = json!({
        "model": model,
        "messages": request.messages.iter().map(message_json).collect::<Vec<_>>(),
    });
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(tool_json).collect();
    }
    if let AnswerKind::Structured { name, schema } = &request.answer {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": name, "schema": schema, "strict": true },
        });
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_completion_tokens"] = json!(max_output_tokens);
    }
    body
}

fn message_json(message: &Message) -> Value {
    match message {
        Message::System(content) => json!({ "role": "system", "content": content }),
        Message::User(content) => json!({ "role": "user", "content": content }),
        Message::Assistant { text, tool_calls } => {
            let mut message = json!({ "role": "assistant", "content": text });
            if !tool_calls.is_empty() {
                message["tool_calls"] = tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments.to_string() },
                        })
                    })
                    .collect();
            }
            message
        }
        Message::ToolResult { call_id, content } => {
            json!({ "role": "tool", "tool_call_id": call_id, "content": content })
        }
    }
}

fn tool_json(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": { "name": tool.name, "description": tool.description, "parameters": tool.parameters },
    })
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
    refusal: Option<String>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

fn read_answer(bytes: &[u8], answer: &AnswerKind) -> Result<ModelResponse, ModelError> {
    let response: ChatResponse = serde_json::from_slice(bytes)
        .map_err(|error| ModelError::BadAnswer(format!("not a chat completion: {error}")))?;
    let usage = response.usage.map_or_else(Usage::default, |usage| Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
    });
    let Some(choice) = response.choices.into_iter().next() else {
        return Err(ModelError::BadAnswer("no choice in the answer".to_owned()));
    };
    if choice.message.refusal.is_some() || choice.finish_reason.as_deref() == Some("content_filter")
    {
        return Err(ModelError::Refused(
            "the model declined to answer".to_owned(),
        ));
    }
    let output = if choice.message.tool_calls.is_empty() {
        let Some(content) = choice.message.content else {
            return Err(ModelError::BadAnswer(
                "the answer has no content".to_owned(),
            ));
        };
        match answer {
            AnswerKind::Text => ModelOutput::Text(content),
            AnswerKind::Structured { .. } => {
                ModelOutput::Structured(serde_json::from_str(&content).map_err(|error| {
                    ModelError::BadAnswer(format!("the structured answer is not JSON: {error}"))
                })?)
            }
        }
    } else {
        ModelOutput::ToolCalls(
            choice
                .message
                .tool_calls
                .into_iter()
                .map(tool_call)
                .collect::<Result<_, _>>()?,
        )
    };
    Ok(ModelResponse { output, usage })
}

fn tool_call(call: WireToolCall) -> Result<ToolCall, ModelError> {
    let arguments = serde_json::from_str(&call.function.arguments).map_err(|error| {
        ModelError::BadAnswer(format!(
            "the arguments of tool `{}` are not JSON: {error}",
            call.function.name
        ))
    })?;
    Ok(ToolCall {
        id: call.id,
        name: call.function.name,
        arguments,
    })
}
