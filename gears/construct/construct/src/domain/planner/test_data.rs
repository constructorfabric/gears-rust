use construct_sdk::person_types::PERSON_TYPES;
use serde_json::{Value, json};
use uuid::Uuid;

use super::catalog::PropertyCatalog;
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
