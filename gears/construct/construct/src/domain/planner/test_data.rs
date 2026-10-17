use std::collections::VecDeque;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use construct_sdk::person_types::PERSON_TYPES;
use serde_json::{Value, json};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::agent::PlannerCaps;
use super::catalog::PropertyCatalog;
use crate::domain::model_client::{
    ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::plan::{NodeKey, Origin};
use crate::domain::profile::{Category, Fact, Profile};

pub(super) const TENANT: Uuid = Uuid::from_u128(0x6b1f_0c2a_93d4_4e8f_a1b2_c3d4_e5f6_0718);
pub(super) const SUBJECT: Uuid = Uuid::from_u128(0x2c9e_7a41_5b3f_4d6e_8f90_1a2b_3c4d_5e6f);

pub(super) fn catalog() -> PropertyCatalog {
    PropertyCatalog::load().expect("the catalog")
}

/// The schema of `property` read straight from the JSON of its person type, following a reference into the type's
/// own definitions.
pub(super) fn schema_in_person_type(property: &str) -> Value {
    PERSON_TYPES
        .iter()
        .map(|person_type| {
            serde_json::from_str::<Value>(person_type.schema).expect("a JSON schema")
        })
        .find_map(|document| {
            let found = document
                .pointer(&format!(
                    "/allOf/1/properties/payload/properties/{property}"
                ))?
                .clone();
            Some(match found["$ref"].as_str() {
                Some(reference) => document
                    .pointer(reference.trim_start_matches('#'))
                    .expect("a definition")
                    .clone(),
                None => found,
            })
        })
        .expect("a property of a person type")
}

pub(super) fn origin() -> Origin {
    Origin::Connector {
        connector: "chat_engine".to_owned(),
        provenance: "chat_engine/thread-42/msg-7".to_owned(),
        version: "v1".to_owned(),
        type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

pub(super) fn profile() -> Profile {
    Profile {
        version: Some(7),
        facts: vec![
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:5d2c8a3e-0f41-4b7a-9c6d-2e8f1a3b4c5d"
                )),
                category: Category::Role,
                property: "role".to_owned(),
                value: json!("teacher"),
            },
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:a7e3f9b1-2c4d-4e6f-8a0b-1c3d5e7f9a2b"
                )),
                category: Category::Skill,
                property: "skills".to_owned(),
                value: json!({ "name": "Python" }),
            },
            Fact {
                node_key: NodeKey(format!(
                    "construct:{TENANT}:c4b6d8e0-1f3a-4c5e-9b7d-0e2f4a6c8e1b"
                )),
                category: Category::Identity,
                property: "full_name".to_owned(),
                value: json!("Jane Doe"),
            },
        ],
    }
}

pub(super) fn node_key(number: usize) -> NodeKey {
    profile().facts[number - 1].node_key.clone()
}

pub(super) fn is_new_key(key: &NodeKey) -> bool {
    key.0.starts_with(&format!("construct:{TENANT}:"))
        && profile().facts.iter().all(|fact| fact.node_key != *key)
}

pub(super) type Answer = Result<ModelResponse, ModelError>;

pub(super) fn caps() -> PlannerCaps {
    PlannerCaps {
        max_rounds: NonZeroU32::new(3).expect("rounds"),
        max_tokens: NonZeroU64::new(10_000).expect("tokens"),
    }
}

pub(super) struct ScriptedModel {
    answers: Mutex<VecDeque<Answer>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    pub(super) fn answering(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            requests: Mutex::default(),
        })
    }

    pub(super) fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(
        &self,
        _: &SecurityContext,
        request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        self.requests.lock().expect("lock").push(request.clone());
        let mut answers = self.answers.lock().expect("lock");
        if answers.len() > 1 {
            answers.pop_front().expect("an answer")
        } else {
            answers.front().cloned().expect("an answer")
        }
    }
}

pub(super) fn usage(input_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens: 20,
    }
}

pub(super) fn tool_calls(calls: &[(&str, Value)], input_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: ModelOutput::ToolCalls(
            calls
                .iter()
                .enumerate()
                .map(|(index, (name, arguments))| ToolCall {
                    id: format!("call_{index}"),
                    name: (*name).to_owned(),
                    arguments: arguments.clone(),
                })
                .collect(),
        ),
        usage: Some(usage(input_tokens)),
    }
}

pub(super) fn done(input_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: ModelOutput::Text("The plan is complete.".to_owned()),
        usage: Some(usage(input_tokens)),
    }
}
