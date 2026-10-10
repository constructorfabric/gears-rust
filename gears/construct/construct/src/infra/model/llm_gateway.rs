use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use llm_gateway_sdk::{
    CreateResponseBody, FunctionCallItem, FunctionCallOutputItem, FunctionTool, InputContent,
    InputItem, LlmGatewayClientV1, LlmGatewayError, MessageItem, OutputContentPart, OutputItem,
    ResponseInput, ResponseResource, ResponseStatus, Role, TextFormat, TextFormatKind, Tool,
};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;

use crate::domain::model_client::{
    AnswerKind, Message, ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse,
    ToolCall, ToolSpec, Usage,
};

const CONTENT_FILTER: &str = "content_filter";

/// @cpt-dod:cpt-cf-construct-dod-model-client-llm-gateway:p1
pub struct LlmGatewayModel {
    hub: Arc<ClientHub>,
    model: String,
    timeout: Duration,
}

impl LlmGatewayModel {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, model: String, timeout: Duration) -> Self {
        Self {
            hub,
            model,
            timeout,
        }
    }
}

#[async_trait]
impl ModelClient for LlmGatewayModel {
    async fn complete(
        &self,
        ctx: &SecurityContext,
        request: ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        let Some(gateway) = self.hub.try_get::<dyn LlmGatewayClientV1>() else {
            return Err(ModelError::Unavailable(
                "no LLM gateway client in ClientHub".to_owned(),
            ));
        };
        let body = request_body(&self.model, &request);
        let response = tokio::time::timeout(self.timeout, gateway.create_response(ctx, body))
            .await
            .map_err(|_| ModelError::Timeout)?
            .map_err(|error| gateway_error(&error))?;
        read_answer(response, &request.answer)
    }
}

fn gateway_error(error: &LlmGatewayError) -> ModelError {
    match error {
        LlmGatewayError::ProviderTimeout => ModelError::Timeout,
        LlmGatewayError::ModelNotFound { .. } => refused("model_not_found"),
        LlmGatewayError::ModelNotApproved { .. } => refused("model_not_approved"),
        LlmGatewayError::Validation { .. } => refused("validation_error"),
        LlmGatewayError::CapabilityNotSupported { .. } => refused("capability_not_supported"),
        LlmGatewayError::BudgetExceeded => refused("budget_exceeded"),
        LlmGatewayError::RequestBlocked { .. } => refused("request_blocked"),
        LlmGatewayError::OutputValidationError { .. } => {
            ModelError::BadAnswer("output_validation_error".to_owned())
        }
        LlmGatewayError::RateLimited => unavailable("rate_limited"),
        LlmGatewayError::HookTimeout => unavailable("hook_timeout"),
        LlmGatewayError::ProviderError { .. } => unavailable("provider_error"),
        LlmGatewayError::Internal { .. } => unavailable("internal"),
        _ => unavailable("unknown gateway error"),
    }
}

fn refused(code: &str) -> ModelError {
    ModelError::Refused(code.to_owned())
}

fn unavailable(code: &str) -> ModelError {
    ModelError::Unavailable(code.to_owned())
}

fn request_body(model: &str, request: &ModelRequest) -> CreateResponseBody {
    CreateResponseBody {
        model: model.to_owned(),
        input: Some(ResponseInput::Items(
            request.messages.iter().flat_map(input_items).collect(),
        )),
        tools: (!request.tools.is_empty())
            .then(|| request.tools.iter().map(function_tool).collect()),
        text: match &request.answer {
            AnswerKind::Text => None,
            AnswerKind::Structured { name, schema } => Some(TextFormat {
                format: TextFormatKind::JsonSchema {
                    name: name.clone(),
                    description: None,
                    schema: Some(schema.clone()),
                    strict: true,
                },
                verbosity: None,
            }),
        },
        max_output_tokens: request.max_output_tokens,
        ..CreateResponseBody::default()
    }
}

fn input_items(message: &Message) -> Vec<InputItem> {
    match message {
        Message::System(content) => vec![message_item(Role::System, content)],
        Message::User(content) => vec![message_item(Role::User, content)],
        Message::Assistant { text, tool_calls } => text
            .iter()
            .map(|text| message_item(Role::Assistant, text))
            .chain(tool_calls.iter().map(|call| {
                InputItem::FunctionCall(FunctionCallItem {
                    id: None,
                    status: None,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.to_string(),
                })
            }))
            .collect(),
        Message::ToolResult { call_id, content } => {
            vec![InputItem::FunctionCallOutput(FunctionCallOutputItem {
                id: None,
                call_id: call_id.clone(),
                output: InputContent::Text(content.clone()),
                status: None,
            })]
        }
    }
}

fn message_item(role: Role, content: &str) -> InputItem {
    InputItem::Message(MessageItem {
        id: None,
        role,
        content: InputContent::Text(content.to_owned()),
        status: None,
    })
}

fn function_tool(tool: &ToolSpec) -> Tool {
    Tool::Function(FunctionTool {
        name: tool.name.clone(),
        description: Some(tool.description.clone()),
        parameters: Some(tool.parameters.clone()),
        strict: None,
    })
}

fn read_answer(
    response: ResponseResource,
    answer: &AnswerKind,
) -> Result<ModelResponse, ModelError> {
    match response.status {
        ResponseStatus::Completed => {}
        ResponseStatus::Incomplete => {
            let reason = response
                .incomplete_details
                .map_or_else(|| "no reason".to_owned(), |details| details.reason);
            return Err(if reason == CONTENT_FILTER {
                ModelError::Refused("the model declined to answer".to_owned())
            } else {
                ModelError::BadAnswer(format!("the answer is incomplete: {reason}"))
            });
        }
        ResponseStatus::Failed => {
            let code = response
                .error
                .map_or_else(|| "no code".to_owned(), |error| error.code);
            return Err(ModelError::Unavailable(format!(
                "the response failed: {code}"
            )));
        }
        ResponseStatus::Queued | ResponseStatus::InProgress => {
            return Err(ModelError::BadAnswer(
                "the response is not completed".to_owned(),
            ));
        }
    }
    let usage = response.usage.map_or_else(Usage::default, |usage| Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
    });

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in response.output {
        match item {
            OutputItem::Message(message) => {
                for part in message.content {
                    match part {
                        OutputContentPart::OutputText(output) => text.push_str(&output.text),
                        OutputContentPart::Refusal(_) => {
                            return Err(ModelError::Refused(
                                "the model declined to answer".to_owned(),
                            ));
                        }
                        OutputContentPart::Other(_) => {}
                    }
                }
            }
            OutputItem::FunctionCall(call) => tool_calls.push(tool_call(call)?),
            OutputItem::Reasoning(_) | OutputItem::Data(_) | OutputItem::Other(_) => {}
        }
    }

    let output = if !tool_calls.is_empty() {
        ModelOutput::ToolCalls(tool_calls)
    } else if text.is_empty() {
        return Err(ModelError::BadAnswer(
            "the answer has no content".to_owned(),
        ));
    } else {
        match answer {
            AnswerKind::Text => ModelOutput::Text(text),
            AnswerKind::Structured { .. } => {
                ModelOutput::Structured(serde_json::from_str(&text).map_err(|error| {
                    ModelError::BadAnswer(format!("the structured answer is not JSON: {error}"))
                })?)
            }
        }
    };
    Ok(ModelResponse { output, usage })
}

fn tool_call(call: FunctionCallItem) -> Result<ToolCall, ModelError> {
    let arguments = serde_json::from_str(&call.arguments).map_err(|error| {
        ModelError::BadAnswer(format!(
            "the arguments of tool `{}` are not JSON: {error}",
            call.name
        ))
    })?;
    Ok(ToolCall {
        id: call.call_id,
        name: call.name,
        arguments,
    })
}
