use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use toolkit_macros::domain_model;

use super::catalog::Catalog;
use crate::domain::model_client::ToolSpec;

/// The name of the tool that adds a node.
pub const ADD: &str = "add";
/// The name of the tool that links two nodes.
pub const LINK: &str = "link";
/// The name of the tool that replaces a node with a new value.
pub const REPLACE: &str = "replace";
/// The name of the tool that removes a node or a link.
pub const REMOVE: &str = "remove";

/// Why a tool call was refused. The model gets the text as the tool result, and the plan does not change.
#[domain_model]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolRefusal {
    #[error("there is no tool `{0}`; use add, link, replace or remove")]
    UnknownTool(String),
    #[error("the arguments do not fit the tool: {0}")]
    BadArguments(String),
    #[error("confidence must be a number from 0 to 1")]
    BadConfidence,
    #[error("there is no line {0} in the profile")]
    UnknownNumber(usize),
    #[error("line {0} already has a step in this plan")]
    NumberUsed(usize),
    #[error("there is no kind `{kind}`; use one of: {known}")]
    UnknownKind { kind: String, known: String },
    #[error("there is no link kind `{kind}`; use one of: {known}")]
    UnknownLinkKind { kind: String, known: String },
    #[error("the value does not fit `{kind}`: {reason}")]
    ValueDoesNotFit { kind: String, reason: String },
    #[error(
        "a `{kind}` is owned by a node of one of these kinds: {owners}; give the number of one as `owner`"
    )]
    OwnerRequired { kind: String, owners: String },
    #[error("a `{kind}` has no owner but the person; leave `owner` out")]
    OwnerNotAllowed { kind: String },
    #[error("line {number} is a `{found}`, and a `{kind}` is owned by one of: {owners}")]
    WrongOwner {
        kind: String,
        number: usize,
        found: String,
        owners: String,
    },
    #[error("line {0} is not a node of the profile")]
    NotANode(usize),
    #[error(
        "the profile already holds this {kind} as {number}; use {number} and link to it, or replace it"
    )]
    AlreadyExists { kind: String, number: usize },
    #[error("this plan already adds this {kind} as {number}; use {number} and link to it")]
    AlreadyAdded { kind: String, number: usize },
    #[error("this plan adds a node owned by line {0}; do not remove line {0} in the same plan")]
    OwnsNewNodes(usize),
    #[error(
        "the person forgot this {kind}, and this message is where it came from; do not add it again"
    )]
    Forgotten { kind: String },
    #[error("the profile holds one `{kind}`, and it is {number}; use replace {number}")]
    SecondNode { kind: String, number: usize },
    #[error("this plan already adds a `{kind}`, and the profile holds one only")]
    SecondNodeInPlan { kind: String },
    #[error("line {number} already holds this; make no change for it")]
    AlreadyHeld { number: usize },
    #[error(
        "a `{link}` link goes from a node of one of these kinds: {from}, to one of: {to}; line {number} is a `{found}`"
    )]
    LinkEnd {
        link: String,
        from: String,
        to: String,
        number: usize,
        found: String,
    },
    #[error("the profile or this plan already has this `{kind}` link between {from} and {to}")]
    LinkExists {
        kind: String,
        from: usize,
        to: usize,
    },
    #[error("line {0} is a link, not a node; remove a link by its number, or give a node's number")]
    NotANodeLink(usize),
}

#[domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddArguments {
    pub kind: String,
    pub value: Value,
    #[serde(default)]
    pub owner: Option<usize>,
    pub confidence: f32,
}

#[domain_model]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkArguments {
    pub kind: String,
    pub from: usize,
    pub to: usize,
    #[serde(default)]
    pub fields: Option<Value>,
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

/// The add, link, replace and remove tools the model may call.
#[must_use]
pub fn tool_specs(catalog: &Catalog) -> Vec<ToolSpec> {
    let confidence = json!({ "type": "number", "minimum": 0, "maximum": 1 });
    let number = json!({ "type": "integer", "minimum": 1 });
    let value = json!({ "description": "The whole value: the object of the node's fields, in the form the kind takes." });
    vec![
        ToolSpec {
            name: ADD.to_owned(),
            description: "Add a new node to the profile. A node of an entry kind is owned by a node: give its number as `owner`. \
                          The answer gives the new node's number, which a link or an owner can use in this plan."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": catalog.kind_names().collect::<Vec<_>>() },
                    "value": value,
                    "owner": { "type": "integer", "minimum": 1 },
                    "confidence": confidence,
                },
                "required": ["kind", "value", "confidence"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: LINK.to_owned(),
            description: "Link two nodes of the profile by a link kind, from the first to the second. Both are given by number."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": catalog.link_names().collect::<Vec<_>>() },
                    "from": number,
                    "to": number,
                    "fields": { "type": "object", "description": "The link's fields, when its kind has any." },
                    "confidence": confidence,
                },
                "required": ["kind", "from", "to", "confidence"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: REPLACE.to_owned(),
            description: "Replace the node with this number by a new value. Send the complete new value: \
                          fields left out are gone. The node keeps its links and what it owns."
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
            description: "Remove the node or the link with this number. A node goes with the nodes it owns and all its links."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "number": number, "confidence": confidence },
                "required": ["number", "confidence"],
                "additionalProperties": false,
            }),
        },
    ]
}
