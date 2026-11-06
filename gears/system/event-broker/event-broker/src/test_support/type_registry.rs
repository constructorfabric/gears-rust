//! Declarative topic/event-type fixtures for `EventBrokerHarness`.
//!
//! Every entry becomes the document `types-registry` would actually hold - a
//! topic as an instance of the topic base type, an event type as a derived type
//! schema of the abstract event base type - and the harness loads them through
//! the same path a booting process uses. Fixtures built as the broker's own
//! structs would let a unit suite pass against a process that cannot load a
//! topic at all.
//!
//! `StaticTypesRegistry::of(...)` takes a flat list; each entry's `id` alone
//! says which kind it is, since there is exactly one base type per kind, and
//! every id is validated as the GTS kind it claims to be.
//!
//! The documents are held by a `MockTypesRegistry` that also holds the
//! broker's two base types, taken from the declarations `event-broker-sdk`
//! links into this binary's GTS inventory, so every listed type schema is
//! resolved the way `types-registry` resolves it. Nothing else from the
//! inventory is held: a gear's own contract is never included. A test that
//! needs it passes a registry holding it through
//! `EventBrokerHarnessBuilder::with_types_registry_client`.

use std::collections::HashSet;
use std::sync::Arc;

use event_broker_sdk::gts::{EventV1, TopicV1};
use gts::GtsTypeId;
use serde_json::{Value as JsonValue, json};
use toolkit_gts::{GTS_ID_URI_PREFIX, GtsInstanceId, GtsSchema, gts_type_schema};
use types_registry_sdk::testing_platform::MockTypesRegistry;

use crate::config::EventBrokerConfig;

const TOPIC_BASE: &str = TopicV1::TYPE_ID;
const EVENT_BASE: &str = EventV1::<()>::TYPE_ID;

/// The backend every fixture deployment names. A fixture never builds it
/// through configuration - the harness builds the memory backend directly - but
/// configuration has to name a backend type for the same reason production does.
const MEMORY_BACKEND: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.memory.v1~");

// The abstract template supplies the event envelope without materializing a
// publishable type from the process inventory. The catalog fills fixture metadata
// and payload constraints on the macro-generated document.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~x.eb.fixtures.template.v1~"),
    description = "Parameterized event fixture envelope.",
    properties = "",
    gts_abstract = true,
)]
#[schemars(extend("additionalProperties" = true))]
struct FixtureEventV1;

/// A registry holding the broker's two base types (`TOPIC_BASE` and
/// `EVENT_BASE`, as `event-broker-sdk` declares them), the `topics` instance
/// documents (keyed by their `id`) and the `event_types` schema documents
/// (keyed by their `$id`). Nothing else from this binary's GTS inventory is
/// held, so a gear's own contract is never included.
///
/// # Panics
///
/// Panics if a base type is missing from the inventory or its schema is not
/// valid JSON, if a topic document names no string `id`, or if an event-type
/// document names no string `$id`.
#[must_use]
pub fn registry_holding(topics: &[JsonValue], event_types: &[JsonValue]) -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    for base in [TOPIC_BASE, EVENT_BASE] {
        let entry = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
            .into_iter()
            .find(|entry| entry.type_id == base)
            .unwrap_or_else(|| panic!("the GTS inventory holds no '{base}' type schema"));
        let schema = serde_json::from_str((entry.schema_fn)().as_str())
            .unwrap_or_else(|err| panic!("the '{base}' type schema is not valid JSON: {err}"));
        registry.seed(base, schema);
    }
    for topic in topics {
        let id = topic["id"].as_str().expect("a topic document names its id");
        registry.seed(id, topic.clone());
    }
    for event_type in event_types {
        let id = event_type["$id"]
            .as_str()
            .expect("an event-type document names its $id");
        registry.seed(
            id.strip_prefix(GTS_ID_URI_PREFIX).unwrap_or(id),
            event_type.clone(),
        );
    }
    registry
}

/// Declarative fixtures, applied by `EventBrokerHarnessBuilder::build()`.
pub struct StaticTypesRegistry {
    /// Topic instance documents, exactly as an owning gear would register them.
    pub(super) topics: Vec<JsonValue>,
    /// Derived event-type schema documents.
    pub(super) event_types: Vec<JsonValue>,
    /// The configuration the partition counts came from: a topic carries no
    /// count of its own, so a fixture that asks for four partitions is asking
    /// for a deployment configured that way.
    pub(super) config: EventBrokerConfig,
}

impl StaticTypesRegistry {
    /// `spec` is a flat JSON array; each entry's `id` prefix decides what
    /// fields it needs:
    /// - a topic (`{TOPIC_BASE}...`, no trailing `~`): `partitions` (integer,
    ///   required) - written into configuration rather than onto the topic,
    ///   because that is where a partition count lives.
    /// - an event type (`{EVENT_BASE}...~`, trailing `~` required, since a
    ///   concrete event type is a derived type schema): `topic` (the `id` of a
    ///   topic declared earlier in the same list, required), `data_schema`
    ///   (object, defaults to `{}`), `allowed_subject_types` (array of GTS
    ///   Type-id pattern strings, defaults to `[]` - the true minimal value,
    ///   which rejects every `subject_type` at publish time), and
    ///   `partition_key` (a JSON Pointer, defaulting to the tenant the base
    ///   declares).
    ///
    /// An event type may name a topic deliberately left unregistered - the only
    /// way to reach the broker's "the type resolves a topic nobody registered"
    /// branch - with `"topic_registered": false`.
    ///
    /// # Panics
    /// Panics (fixture-construction failure, not a runtime `Result`) if `spec`
    /// is not a JSON array; an entry is missing `id` or a kind-required field;
    /// an `id` is not well-formed for its kind; an `id`'s base type is neither
    /// the topic nor the event base; or an event type's `topic` was not declared
    /// earlier in the same list.
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
        let mut settings = serde_json::Map::new();

        for entry in entries {
            let id = entry["id"].as_str().unwrap_or_else(|| {
                panic!("StaticTypesRegistry entry missing a string 'id': {entry}")
            });

            if id.starts_with(TOPIC_BASE) && !id.ends_with('~') {
                GtsInstanceId::try_new(id)
                    .unwrap_or_else(|err| panic!("'{id}' is not a valid GTS instance id: {err}"));
                let partitions = entry["partitions"].as_i64().unwrap_or_else(|| {
                    panic!("topic '{id}' is missing an integer 'partitions' field")
                });
                topic_ids.insert(id.to_owned());
                topics.push(json!({
                    "id": id,
                    "description": "a topic this test publishes to",
                }));
                settings.insert(id.to_owned(), json!({ "partitions": partitions }));
            } else if id.starts_with(EVENT_BASE) {
                GtsTypeId::try_new(id).unwrap_or_else(|err| {
                    panic!(
                        "'{id}' is not a valid GTS type id: {err} - a concrete event type is a \
                         derived type schema, so its identifier ends in '~'"
                    )
                });
                let topic = entry["topic"]
                    .as_str()
                    .unwrap_or_else(|| panic!("event type '{id}' is missing a string 'topic'"));
                let topic_registered = entry
                    .get("topic_registered")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(true);
                assert!(
                    !topic_registered || topic_ids.contains(topic),
                    "event type '{id}' references topic '{topic}', which isn't declared earlier \
                     in this StaticTypesRegistry::of(...) list - add \"topic_registered\": false \
                     if that is the point of the fixture"
                );
                let allowed: Vec<&str> = entry
                    .get("allowed_subject_types")
                    .map(|value| {
                        value
                            .as_array()
                            .unwrap_or_else(|| {
                                panic!("event type '{id}' 'allowed_subject_types' must be an array")
                            })
                            .iter()
                            .map(|entry| {
                                entry.as_str().unwrap_or_else(|| {
                                    panic!(
                                        "event type '{id}' 'allowed_subject_types' entries must \
                                         be strings"
                                    )
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let data_schema = entry.get("data_schema").cloned().unwrap_or(json!({}));
                let mut body = FixtureEventV1::gts_schema_with_refs();
                let envelope = body
                    .as_object_mut()
                    .expect("the macro emits an object schema");
                envelope.remove("description");
                envelope.remove("x-gts-abstract");
                body["$id"] = json!(format!("{GTS_ID_URI_PREFIX}{id}"));
                body["x-gts-traits"] = json!({
                    "topic": topic,
                    "allowed_subject_types": allowed,
                    "partition_key": "/tenant_id",
                });
                body["allOf"][1]["properties"]["data"] = if data_schema
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
                {
                    EventV1::<()>::gts_schema_with_refs()["properties"]["data"].clone()
                } else {
                    data_schema
                };
                // The standard builder fixes the pointer at the tenant. An
                // entry may override it, which is how a fixture exercises a
                // type that groups by a payload member.
                if let Some(pointer) = entry.get("partition_key").and_then(JsonValue::as_str) {
                    body["x-gts-traits"]["partition_key"] = json!(pointer);
                }
                event_types.push(body);
            } else {
                panic!(
                    "'{id}' is neither a topic instance ('{TOPIC_BASE}...') nor an event type \
                     ('{EVENT_BASE}...~') - StaticTypesRegistry seeds these two kinds only"
                );
            }
        }

        Self {
            topics,
            event_types,
            config: serde_json::from_value(json!({
                "mode": "standalone",
                "backends": { "main": { "type": MEMORY_BACKEND } },
                "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
                "topics": settings,
            }))
            .expect("fixture configuration deserializes"),
        }
    }

    /// Generated event-type schema documents held by this fixture catalog.
    #[must_use]
    pub fn event_type_schemas(&self) -> &[JsonValue] {
        &self.event_types
    }

    /// The configuration a fixture-free harness runs with: no topics named, so
    /// every topic resolves to the built-in tier.
    #[must_use]
    pub fn empty_config() -> EventBrokerConfig {
        serde_json::from_value(json!({
            "mode": "standalone",
            "backends": { "main": { "type": MEMORY_BACKEND } },
            "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
            "topics": {},
        }))
        .expect("fixture configuration deserializes")
    }
}
