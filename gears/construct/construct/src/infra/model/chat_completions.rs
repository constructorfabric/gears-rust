use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{Method, StatusCode};
use oagw_sdk::{Body, ServiceGatewayClientV1, ServiceGatewayError};
use serde::Deserialize;
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;

use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse,
    ToolCall, ToolSpec, Usage,
};

const LENGTH: &str = "length";
const CONTENT_FILTER: &str = "content_filter";

/// The model client over the `OpenAI` chat completions API, posted through OAGW to one upstream.
///
/// The OAGW client is resolved from `ClientHub` on each call, so the gear does not depend on the start order.
///
/// @cpt-dod:cpt-cf-construct-dod-model-client-chat-completions:p1
pub struct ChatCompletionsModel {
    hub: Arc<ClientHub>,
    upstream_alias: String,
    model: String,
    timeout: Duration,
}

impl std::fmt::Debug for ChatCompletionsModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatCompletionsModel")
            .field("upstream_alias", &self.upstream_alias)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl ChatCompletionsModel {
    /// A client that sends `model` to the OAGW upstream `upstream_alias`, with `timeout` for the whole call.
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
        request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        let Some(gateway) = self.hub.try_get::<dyn ServiceGatewayClientV1>() else {
            return Err(ModelError::Unavailable(
                "no OAGW client in ClientHub".to_owned(),
            ));
        };
        let body = serde_json::to_vec(&request_body(&self.model, request))
            .map_err(|_| ModelError::BadAnswer("the request could not be encoded".to_owned()))?;
        let http_request = http::Request::builder()
            .method(Method::POST)
            .uri(format!("/{}", self.upstream_alias))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(Body::Bytes(Bytes::from(body)))
            .map_err(|_| ModelError::Refused("the request could not be built".to_owned()))?;

        let call = async {
            let response = gateway
                .proxy_request(ctx.clone(), http_request)
                .await
                .map_err(|error| gateway_error(&ServiceGatewayError::from(error)))?;
            let (parts, body) = response.into_parts();
            let bytes = body
                .into_bytes()
                .await
                .map_err(|_| ModelError::Unavailable("the answer could not be read".to_owned()))?;
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

fn gateway_error(error: &ServiceGatewayError) -> ModelError {
    let refused = |variant: &str| ModelError::Refused(format!("OAGW {variant}"));
    match error {
        ServiceGatewayError::Timeout => ModelError::Timeout,
        ServiceGatewayError::RateLimited { .. } => {
            ModelError::Unavailable("OAGW RateLimited".to_owned())
        }
        ServiceGatewayError::Unavailable { .. } => {
            ModelError::Unavailable("OAGW Unavailable".to_owned())
        }
        ServiceGatewayError::AuthFailed { .. } => refused("AuthFailed"),
        ServiceGatewayError::PermissionDenied { .. } => refused("PermissionDenied"),
        ServiceGatewayError::PayloadTooLarge { .. } => refused("PayloadTooLarge"),
        ServiceGatewayError::InvalidTargetHost { .. } => refused("InvalidTargetHost"),
        ServiceGatewayError::Validation { .. } => refused("Validation"),
        ServiceGatewayError::NotFound { .. } => refused("NotFound"),
        ServiceGatewayError::AlreadyExists { .. } => refused("AlreadyExists"),
        ServiceGatewayError::FailedPrecondition { .. } => refused("FailedPrecondition"),
        ServiceGatewayError::Aborted { .. } => refused("Aborted"),
        ServiceGatewayError::Internal { .. } => refused("Internal"),
        ServiceGatewayError::Other { .. } => refused("Other"),
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
    tool_calls: Option<Vec<WireToolCall>>,
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

fn bad_answer(reason: &str) -> ModelError {
    ModelError::BadAnswer(reason.to_owned())
}

fn declined() -> ModelError {
    ModelError::Refused("the model declined to answer".to_owned())
}

fn read_answer(bytes: &[u8], answer: &AnswerKind) -> Result<ModelResponse, ModelError> {
    let response: ChatResponse =
        serde_json::from_slice(bytes).map_err(|_| bad_answer("not a chat completion"))?;
    let usage = response.usage.map(|usage| Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
    });
    let Some(choice) = response.choices.into_iter().next() else {
        return Err(bad_answer("no choice in the answer"));
    };
    match choice.finish_reason.as_deref() {
        Some(CONTENT_FILTER) => return Err(declined()),
        Some(LENGTH) => return Err(bad_answer("the answer was cut off at its token limit")),
        _ => {}
    }
    if choice.message.refusal.is_some() {
        return Err(declined());
    }
    let tool_calls = choice.message.tool_calls.unwrap_or_default();
    let output = if tool_calls.is_empty() {
        let content = choice
            .message
            .content
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| bad_answer("the answer has no content"))?;
        match answer {
            AnswerKind::Text => ModelOutput::Text(content),
            AnswerKind::Structured { .. } => ModelOutput::Structured(
                serde_json::from_str(&content)
                    .map_err(|_| bad_answer("the structured answer is not JSON"))?,
            ),
        }
    } else {
        ModelOutput::ToolCalls(
            tool_calls
                .into_iter()
                .enumerate()
                .map(|(index, call)| tool_call(index, call))
                .collect::<Result<_, _>>()?,
        )
    };
    Ok(ModelResponse { output, usage })
}

fn tool_call(index: usize, call: WireToolCall) -> Result<ToolCall, ModelError> {
    let arguments = serde_json::from_str(&call.function.arguments).map_err(|_| {
        ModelError::BadAnswer(format!("the arguments of tool call {index} are not JSON"))
    })?;
    Ok(ToolCall {
        id: call.id,
        name: call.function.name,
        arguments,
    })
}
