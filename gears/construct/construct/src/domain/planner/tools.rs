use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use toolkit_macros::domain_model;

use super::catalog::PropertyCatalog;
use crate::domain::model_client::ToolSpec;

/// The name of the tool that adds a new value of a property.
pub const ADD: &str = "add";
/// The name of the tool that replaces a profile value with a new one.
pub const REPLACE: &str = "replace";
/// The name of the tool that removes a profile value.
pub const REMOVE: &str = "remove";

/// Why a tool call was refused. The model gets the text as the tool result, and the plan does not change.
#[domain_model]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolRefusal {
    #[error("there is no tool `{0}`; use add, replace or remove")]
    UnknownTool(String),
    #[error("the arguments do not fit the tool: {0}")]
    BadArguments(String),
    #[error("confidence must be a number from 0 to 1")]
    BadConfidence,
    #[error("there is no value {0} in the profile")]
    UnknownNumber(usize),
    #[error("value {0} already has a step in this plan")]
    NumberUsed(usize),
    #[error("there is no property `{property}`; use one of: {known}")]
    UnknownProperty { property: String, known: String },
    #[error("the value does not fit `{property}`: {reason}")]
    ValueDoesNotFit { property: String, reason: String },
    #[error("`{property}` holds one value, and value {number} is it; use replace {number}")]
    SecondValue { property: String, number: usize },
    #[error("`{property}` holds one value, and this plan already adds or replaces it")]
    SecondValueInPlan { property: String },
    #[error("value {number} already holds this; make no change for it")]
    AlreadyHeld { number: usize },
    #[error("this plan already writes this value to `{property}`")]
    AlreadyAdded { property: String },
}

#[domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddArguments {
    pub property: String,
    pub value: Value,
    pub confidence: f32,
}

#[domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplaceArguments {
    pub number: usize,
    pub value: Value,
    pub confidence: f32,
}

#[domain_model]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveArguments {
    pub number: usize,
    pub confidence: f32,
}

/// The longest text of the model's own that a refusal repeats.
const ECHO_LIMIT: usize = 80;

/// `text` cut to [`ECHO_LIMIT`] characters, for a refusal that repeats what the model sent.
pub(super) fn echoed(text: &str) -> String {
    if text.chars().count() <= ECHO_LIMIT {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(ECHO_LIMIT).collect::<String>())
    }
}

pub(super) fn parse_arguments<T: DeserializeOwned>(arguments: &Value) -> Result<T, ToolRefusal> {
    T::deserialize(arguments).map_err(|error| ToolRefusal::BadArguments(echoed(&error.to_string())))
}

/// The add, replace and remove tools the model may call.
#[must_use]
pub fn tool_specs(catalog: &PropertyCatalog) -> Vec<ToolSpec> {
    let confidence = json!({ "type": "number", "minimum": 0, "maximum": 1 });
    let number = json!({ "type": "integer", "minimum": 1 });
    let value = json!({ "description": "The whole value, in the form the property takes." });
    vec![
        ToolSpec {
            name: ADD.to_owned(),
            description: "Add a new value of a property to the profile.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "property": { "type": "string", "enum": catalog.names().collect::<Vec<_>>() },
                    "value": value,
                    "confidence": confidence,
                },
                "required": ["property", "value", "confidence"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: REPLACE.to_owned(),
            description: "Replace the profile value with this number by a new value. Send the complete new value: \
                          fields left out are gone."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "number": number, "value": value, "confidence": confidence },
                "required": ["number", "value", "confidence"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: REMOVE.to_owned(),
            description: "Remove the profile value with this number.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "number": number, "confidence": confidence },
                "required": ["number", "confidence"],
                "additionalProperties": false,
            }),
        },
    ]
}
