use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::model_client::{AnswerKind, Message, ModelRequest, ToolCall, ToolSpec};
use crate::test_support::context_in;

pub(super) fn context() -> SecurityContext {
    context_in(Uuid::new_v4())
}

pub(super) fn planner_request() -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::System("Decide how the record changes the profile.".to_owned()),
            Message::User("1. work_history: Acme, teacher".to_owned()),
            Message::Assistant {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_owned(),
                    name: "replace".to_owned(),
                    arguments: json!({ "value_number": 1 }),
                }],
            },
            Message::ToolResult {
                call_id: "call_1".to_owned(),
                content: "replaced value 1".to_owned(),
            },
        ],
        tools: vec![ToolSpec {
            name: "add".to_owned(),
            description: "Add a fact.".to_owned(),
            parameters: json!({ "type": "object", "properties": { "property": { "type": "string" } } }),
        }],
        answer: AnswerKind::Text,
        max_output_tokens: Some(512),
    }
}

pub(super) fn verdict_request() -> ModelRequest {
    ModelRequest {
        messages: vec![Message::User("Check the values.".to_owned())],
        tools: vec![],
        answer: AnswerKind::Structured {
            name: "verdict".to_owned(),
            schema: json!({ "type": "object", "properties": { "verdict": { "type": "string" } } }),
        },
        max_output_tokens: None,
    }
}
