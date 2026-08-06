//! Declarative topic/event-type fixtures for `EventBrokerHarness`, replacing
//! hand-built `Topic`/`EventType` struct literals (which is how every test
//! in this crate ended up using invalid GTS ids like
//! `"gts.example.topic.v1~t1"` - a 4-component instance suffix has no valid
//! GTS shape, and nothing validated it). `StaticTypesRegistry::of(...)`
//! takes a flat list of entries; each entry's `id` alone says which entity
//! kind it is (there is exactly one fixed base type per kind - `Topic`,
//! `EventType` - so no separate `"topics"`/`"event_types"` grouping key is
//! needed), and every id is validated at runtime via
//! `toolkit_gts::GtsInstanceId::try_new` regardless of how the test wrote
//! it (a literal, a `format!()`-built loop variable, anything).

use std::collections::HashSet;

use chrono::Utc;
use event_broker_sdk::gts::{EventTypeV1, TopicV1};
use serde_json::Value as JsonValue;
use toolkit_gts::{GtsInstanceId, GtsSchema};

use crate::domain::model::{EventType, Topic};

const TOPIC_BASE: &str = TopicV1::TYPE_ID;
const EVENT_TYPE_BASE: &str = EventTypeV1::TYPE_ID;

/// Declarative topic/event-type fixtures, applied to `InMemoryDomainRepo`
/// by `EventBrokerHarnessBuilder::build()`.
pub struct StaticTypesRegistry {
    pub(super) topics: Vec<Topic>,
    pub(super) event_types: Vec<EventType>,
}

impl StaticTypesRegistry {
    /// `spec` is a flat JSON array; each entry's `id` prefix (the fixed
    /// `Topic`/`EventType` GTS base type) determines what fields the entry
    /// needs:
    /// - `Topic` (`{TOPIC_BASE}...`): `partitions` (integer, required).
    /// - `EventType` (`{EVENT_TYPE_BASE}...`): `topic` (the `id` of a topic
    ///   declared earlier in the same list, required), `data_schema`
    ///   (object, defaults to `{}`), `allowed_subject_types` (array of GTS
    ///   Type-id pattern strings, defaults to `[]` - the true minimal value,
    ///   which rejects every `subject_type` at publish time; tests that
    ///   publish through this event type must supply a real value).
    ///
    /// # Panics
    /// Panics (test-fixture-construction failure, not a runtime `Result`)
    /// if: `spec` isn't a JSON array; an entry is missing `id` or a
    /// kind-required field; an `id` fails `GtsInstanceId::try_new` (not a
    /// well-formed GTS instance id); an `id`'s base type is neither
    /// `Topic` nor `EventType`; or an `EventType`'s `topic` doesn't match
    /// an `id` already declared earlier in the same `spec`.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "taking JsonValue by value lets every call site pass a bare `json!(...)` literal"
    )]
    pub fn of(spec: JsonValue) -> Self {
        let entries = spec
            .as_array()
            .expect("StaticTypesRegistry::of expects a JSON array");
        let mut topics = Vec::new();
        let mut event_types = Vec::new();
        let mut topic_ids: HashSet<String> = HashSet::new();

        for entry in entries {
            let id = entry["id"].as_str().unwrap_or_else(|| {
                panic!("StaticTypesRegistry entry missing a string 'id': {entry}")
            });
            let gts_id = GtsInstanceId::try_new(id)
                .unwrap_or_else(|err| panic!("'{id}' is not a valid GTS instance id: {err}"));

            if id.starts_with(TOPIC_BASE) {
                let partitions = entry["partitions"].as_i64().unwrap_or_else(|| {
                    panic!("topic '{id}' is missing an integer 'partitions' field")
                });
                topic_ids.insert(id.to_owned());
                topics.push(Topic {
                    id: gts_id,
                    description: None,
                    partitions: i32::try_from(partitions)
                        .unwrap_or_else(|_| panic!("topic '{id}' 'partitions' out of i32 range")),
                    streaming: None,
                    retention: None,
                    created_at: Utc::now(),
                });
            } else if id.starts_with(EVENT_TYPE_BASE) {
                let topic_id_str = entry["topic"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("event type '{id}' is missing a string 'topic' field")
                    })
                    .to_owned();
                assert!(
                    topic_ids.contains(&topic_id_str),
                    "event type '{id}' references topic '{topic_id_str}', which isn't declared \
                     earlier in this StaticTypesRegistry::of(...) list"
                );
                let topic_id = GtsInstanceId::try_new(&topic_id_str).unwrap_or_else(|err| {
                    panic!("event type '{id}' references topic '{topic_id_str}', which is not a valid GTS instance id: {err}")
                });
                let data_schema = entry
                    .get("data_schema")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let allowed_subject_types = entry
                    .get("allowed_subject_types")
                    .map(|v| {
                        v.as_array()
                            .unwrap_or_else(|| {
                                panic!("event type '{id}' 'allowed_subject_types' must be an array")
                            })
                            .iter()
                            .map(|s| {
                                s.as_str()
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "event type '{id}' 'allowed_subject_types' entries must be strings"
                                        )
                                    })
                                    .to_owned()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                event_types.push(EventType {
                    id: gts_id,
                    topic_id,
                    description: None,
                    allowed_subject_types,
                    data_schema,
                    created_at: Utc::now(),
                });
            } else {
                panic!(
                    "'{id}' is neither a Topic ('{TOPIC_BASE}...') nor an EventType \
                     ('{EVENT_TYPE_BASE}...') - StaticTypesRegistry only seeds these two entity \
                     kinds"
                );
            }
        }

        Self {
            topics,
            event_types,
        }
    }
}
