use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{Method, StatusCode};
use oagw_sdk::{Body, ServiceGatewayClientV1, ServiceGatewayError};
use serde::Deserialize;
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;

use jsonschema::Validator;

use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse,
    ToolCall, ToolSpec, Usage,
};

const LENGTH: &str = "length";
const CONTENT_FILTER: &str = "content_filter";

/// The most bytes of a successful answer the client reads.
pub(crate) const ANSWER_LIMIT: usize = 1 << 20;

/// Where the chat completions client sends its calls.
#[derive(Debug, Clone)]
pub struct ChatCompletionsSettings {
    /// The OAGW upstream alias: one plain path segment.
    pub upstream_alias: String,
    /// The model name the upstream expects.
    pub model: String,
    /// The limit for the whole call.
    pub timeout: Duration,
}

/// An OAGW upstream alias that is not one plain path segment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the OAGW upstream alias {0:?} is not one plain path segment")]
pub struct InvalidUpstreamAlias(pub String);

/// The model client over the `OpenAI` chat completions API, posted through OAGW to one upstream.
///
/// The OAGW client is resolved from `ClientHub` on each call, so the gear does not depend on the start order.
///
/// @cpt-dod:cpt-cf-construct-dod-model-client-chat-completions:p1
pub struct ChatCompletionsModel {
    hub: Arc<ClientHub>,
    settings: ChatCompletionsSettings,
}

impl std::fmt::Debug for ChatCompletionsModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatCompletionsModel")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl ChatCompletionsModel {
    /// A client that sends the settings' model to their OAGW upstream.
    ///
    /// # Errors
    ///
    /// [`InvalidUpstreamAlias`] when the alias is empty or holds `/`, `?`, `#` or whitespace.
    pub fn new(
        hub: Arc<ClientHub>,
        settings: ChatCompletionsSettings,
    ) -> Result<Self, InvalidUpstreamAlias> {
        let alias = &settings.upstream_alias;
        if alias.is_empty()
            || alias.contains(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
        {
            return Err(InvalidUpstreamAlias(alias.clone()));
        }
        Ok(Self { hub, settings })
    }
}

#[async_trait]
impl ModelClient for ChatCompletionsModel {
    #[tracing::instrument(
        skip_all,
        fields(
            upstream_alias = %self.settings.upstream_alias,
            model = %self.settings.model,
            status,
            duration_ms,
            input_tokens,
            output_tokens,
        )
    )]
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        // cancel-safe: a dropped call leaves no local state; the upstream may still finish it.
        let Some(gateway) = self.hub.try_get::<dyn ServiceGatewayClientV1>() else {
            return Err(ModelError::Unavailable(
                "no OAGW client in ClientHub".to_owned(),
            ));
        };
        let validator = answer_validator(request)?;
        let http_request = http::Request::builder()
            .method(Method::POST)
            .uri(format!("/{}", self.settings.upstream_alias))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(Body::Bytes(Bytes::from(
                request_body(&self.settings.model, request)
                    .to_string()
                    .into_bytes(),
            )))
            .map_err(|_| invalid_request("the HTTP request could not be built"))?;

        let span = tracing::Span::current();
        let started = Instant::now();
        let call = async {
            let response = gateway
                .proxy_request(ctx.clone(), http_request)
                .await
                .map_err(|error| gateway_error(&ServiceGatewayError::from(error)))?;
            let (parts, body) = response.into_parts();
            span.record("status", parts.status.as_u16());
            if !parts.status.is_success() {
                return Err(status_error(parts.status));
            }
            read_capped(body).await
        };
        let bytes = tokio::time::timeout(self.settings.timeout, call).await;
        span.record(
            "duration_ms",
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let bytes = bytes.map_err(|_| ModelError::Timeout)??;

        let response = read_answer(&bytes, validator.as_ref())?;
        if let Some(usage) = &response.usage {
            span.record("input_tokens", usage.input_tokens);
            span.record("output_tokens", usage.output_tokens);
        }
        Ok(response)
    }
}

fn invalid_request(reason: &str) -> ModelError {
    ModelError::InvalidRequest(reason.to_owned())
}

/// The schema check for a structured answer, or `None` for a text answer.
fn answer_validator(request: &ModelRequest) -> Result<Option<Validator>, ModelError> {
    let AnswerKind::Structured { schema, .. } = &request.answer else {
        return Ok(None);
    };
    if !request.tools.is_empty() {
        return Err(invalid_request("tools with a structured answer"));
    }
    jsonschema::validator_for(schema)
        .map(Some)
        .map_err(|_| invalid_request("the answer schema does not compile"))
}

async fn read_capped(body: Body) -> Result<Vec<u8>, ModelError> {
    let mut stream = body.into_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| ModelError::Unavailable("the answer could not be read".to_owned()))?;
        if bytes.len() + chunk.len() > ANSWER_LIMIT {
            return Err(bad_answer("the answer is larger than its limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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

fn read_answer(bytes: &[u8], structured: Option<&Validator>) -> Result<ModelResponse, ModelError> {
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
    if choice
        .message
        .refusal
        .is_some_and(|refusal| !refusal.trim().is_empty())
    {
        return Err(declined());
    }
    let tool_calls = choice.message.tool_calls.unwrap_or_default();
    let output = if tool_calls.is_empty() {
        let content = choice
            .message
            .content
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| bad_answer("the answer has no content"))?;
        match structured {
            None => ModelOutput::Text(content),
            Some(validator) => {
                let value: Value = serde_json::from_str(&content)
                    .map_err(|_| bad_answer("the structured answer is not JSON"))?;
                if !validator.is_valid(&value) {
                    return Err(bad_answer(
                        "the structured answer does not match its schema",
                    ));
                }
                ModelOutput::Structured(value)
            }
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
