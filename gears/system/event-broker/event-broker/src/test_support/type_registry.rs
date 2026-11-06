//! Declarative topic/event-type fixtures for `EventBrokerHarness`.
//!
//! Every entry becomes the document `types-registry` would actually hold - a
//! topic as an instance of the topic base type, an event type as a derived type
//! schema of the abstract event base type - and the harness loads them through
//! the same path a booting process uses. Fixtures built as the broker's own
//! structs would let a unit suite pass against a process that cannot load a
//! topic at all.
//!
//! Topics are data: `StaticTypesRegistry::of(...)` takes a flat list of topic
//! entries. Event types are Rust declarations - `#[gts_type_schema(base =
//! EventV1, ..)]` structs, added with [`StaticTypesRegistry::event_type`] - so a
//! fixture holds exactly the document the macro generates for a real gear's
//! event type. A fixture whose point is a document the macro cannot produce (one
//! the broker or the registry must refuse) adds it with
//! [`StaticTypesRegistry::refused_event_type_document`].
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
use serde_json::{Value as JsonValue, json};
use toolkit_gts::{GTS_ID_URI_PREFIX, GtsInstanceId, GtsSchema};
use types_registry_sdk::testing_platform::MockTypesRegistry;

use crate::config::EventBrokerConfig;

const TOPIC_BASE: &str = TopicV1::TYPE_ID;
const EVENT_BASE: &str = EventV1::<()>::TYPE_ID;

/// The backend every fixture deployment names. A fixture never builds it
/// through configuration - the harness builds the memory backend directly - but
/// configuration has to name a backend type for the same reason production does.
const MEMORY_BACKEND: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.memory.v1~");

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
    /// Ids of the topics `of(...)` declared, which an event type's `topic`
    /// trait must name.
    topic_ids: HashSet<String>,
    /// The configuration the partition counts came from: a topic carries no
    /// count of its own, so a fixture that asks for four partitions is asking
    /// for a deployment configured that way.
    pub(super) config: EventBrokerConfig,
}

impl StaticTypesRegistry {
    /// `spec` is a flat JSON array of topic entries: `id` (a topic instance id,
    /// `{TOPIC_BASE}...`, no trailing `~`) and `partitions` (integer, required) -
    /// written into configuration rather than onto the topic, because that is
    /// where a partition count lives. Event types are added afterwards with
    /// [`Self::event_type`].
    ///
    /// # Panics
    /// Panics (fixture-construction failure, not a runtime `Result`) if `spec`
    /// is not a JSON array, an entry is missing `id` or `partitions`, an `id` is
    /// not a well-formed topic instance id, or an entry is an event type.
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
        let mut topic_ids: HashSet<String> = HashSet::new();
        let mut settings = serde_json::Map::new();
        for entry in entries {
            let id = entry["id"].as_str().unwrap_or_else(|| {
                panic!("StaticTypesRegistry entry missing a string 'id': {entry}")
            });
            assert!(
                !id.starts_with(EVENT_BASE),
                "'{id}' is an event type: declare it as a `#[gts_type_schema(base = EventV1)]` \
                 struct and add it with `.event_type::<T>()`"
            );
            assert!(
                id.starts_with(TOPIC_BASE) && !id.ends_with('~'),
                "'{id}' is not a topic instance ('{TOPIC_BASE}...') - StaticTypesRegistry::of \
                 seeds topics only"
            );
            GtsInstanceId::try_new(id)
                .unwrap_or_else(|err| panic!("'{id}' is not a valid GTS instance id: {err}"));
            let partitions = entry["partitions"]
                .as_i64()
                .unwrap_or_else(|| panic!("topic '{id}' is missing an integer 'partitions' field"));
            topic_ids.insert(id.to_owned());
            topics.push(json!({
                "id": id,
                "description": "a topic this test publishes to",
            }));
            settings.insert(id.to_owned(), json!({ "partitions": partitions }));
        }

        Self {
            topics,
            event_types: Vec::new(),
            topic_ids,
            config: serde_json::from_value(json!({
                "mode": "standalone",
                "backends": { "main": { "type": MEMORY_BACKEND } },
                "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
                "topics": settings,
            }))
            .expect("fixture configuration deserializes"),
        }
    }

    /// Adds the event type `T` declares - a `#[gts_type_schema(base = EventV1)]`
    /// struct - as the document `types-registry` holds for it. Its `topic` trait
    /// must name a topic declared in [`Self::of`].
    ///
    /// # Panics
    /// Panics if `T` is not an event type or its topic was not declared.
    #[must_use]
    pub fn event_type<T: GtsSchema>(self) -> Self {
        self.add_event_type(T::gts_schema_with_refs(), true)
    }

    /// As [`Self::event_type`], for a type whose topic is deliberately left
    /// unregistered - the only way to reach the broker's "the type resolves a
    /// topic nobody registered" branch.
    ///
    /// # Panics
    /// Panics if `T` is not an event type.
    #[must_use]
    pub fn event_type_on_unregistered_topic<T: GtsSchema>(self) -> Self {
        self.add_event_type(T::gts_schema_with_refs(), false)
    }

    /// Adds a hand-written event-type document, for a fixture whose point is a
    /// document the macro cannot produce: one the broker or the registry must
    /// refuse. Every other event type is a Rust declaration.
    ///
    /// # Panics
    /// Panics if `document` names no event-type `$id`.
    #[must_use]
    pub fn refused_event_type_document(self, document: JsonValue) -> Self {
        self.add_event_type(document, false)
    }

    fn add_event_type(mut self, document: JsonValue, topic_must_be_declared: bool) -> Self {
        let id = document["$id"]
            .as_str()
            .and_then(|uri| uri.strip_prefix(GTS_ID_URI_PREFIX))
            .unwrap_or_else(|| panic!("an event-type document names no `gts://` $id: {document}"));
        assert!(
            id.starts_with(EVENT_BASE) && id != EVENT_BASE,
            "'{id}' is not a type derived from '{EVENT_BASE}'"
        );
        if topic_must_be_declared {
            let topic = document["x-gts-traits"]["topic"].as_str().unwrap_or("");
            assert!(
                self.topic_ids.contains(topic),
                "event type '{id}' names topic '{topic}', which this fixture's \
                 StaticTypesRegistry::of(...) list does not declare - use \
                 `.event_type_on_unregistered_topic::<T>()` if that is the point of the fixture"
            );
        }
        self.event_types.push(document);
        self
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

/// Declares a fixture event type admitting any object payload (an open,
/// body-less `#[gts_type_schema(base = EventV1)]` type), for
/// [`StaticTypesRegistry::event_type`]. Tests exercise the broker, not
/// payloads, so only the identity and the traits vary:
///
/// ```ignore
/// use event_broker_sdk::gts::EventV1; // the macro names `EventV1` as the base
///
/// event_broker::fixture_event_type!(
///     OrderPlacedV1 = "cf.core.events.event.v1~x.eb.t1.placed.v1~",
///     topic = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
///     subjects = ["gts.x.eb.t1.subject.v1~"],
///     partition_key = "/subject",
/// );
/// ```
///
/// `subjects` and `partition_key` are optional; omitted, the type declares no
/// subject types and inherits the base's `/tenant_id` partition key. The
/// calling crate needs `toolkit-gts`, `gts`, `schemars`, `serde` and
/// `serde_json`, which the generated declaration names.
#[macro_export]
macro_rules! fixture_event_type {
    (
        $(#[$attr:meta])*
        $vis:vis $name:ident = $id:literal,
        topic = $topic:expr
        $(, subjects = [$($subject:expr),* $(,)?])?
        $(, partition_key = $pointer:expr)?
        $(,)?
    ) => {
        #[toolkit_gts::gts_type_schema(
            dir_path = "schemas",
            base = EventV1,
            type_id = toolkit_gts::gts_id!($id),
            description = "A fixture event type admitting any object payload.",
            properties = "",
            traits = serde_json::json!({
                "topic": $topic,
                $("allowed_subject_types": [$($subject),*],)?
                $("partition_key": $pointer,)?
            }),
        )]
        #[schemars(extend("additionalProperties" = true))]
        $(#[$attr])*
        $vis struct $name;
    };
}
