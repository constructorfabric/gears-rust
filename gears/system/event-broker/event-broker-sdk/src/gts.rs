//! GTS type definitions the broker owns.
//!
//! Three base types are declared here, and the broker declares nothing else. A
//! concrete topic is a well-known **instance** of [`TopicV1`]; a concrete event
//! type is a **derived type schema** under [`EventV1`], and a published event is
//! an instance of that derived type; a gear may declare the subject kinds it owns
//! as **derived type schemas** under [`SubjectV1`], and `subject_type` accepts any
//! other GTS type id too. Each is registered in `types-registry` by whichever gear
//! owns it, at that gear's init - never by the broker.
//!
//! These declarations are the source of truth for the base schemas: the
//! process-wide `toolkit-gts` inventory seeds `types-registry` from what the
//! macro emits here, not from `docs/schemas/`. The base files under
//! `docs/schemas/` are generated from this module and held to it by
//! `gts_tests`, so per-field keywords live in the schemars attributes below.

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsSchema, GtsTypeId, SELF_ID_REF};
use schemars::JsonSchema;
use serde::Serialize;
use toolkit_gts::{GtsTraitsSchema, gts_id, gts_type_schema};
use toolkit_utils::iso8601_duration::Iso8601Duration;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::sequence::Sequence;

/// ASCII-printable, the platform convention for every event field except `data`.
const ASCII_PRINTABLE: &str = r"^[\x20-\x7E]+$";

/// Printable ASCII with an empty value permitted, for a field whose presence is
/// governed by `required` rather than by its own pattern. A `description` is
/// optional on some resources and required on others; the encoding rule is the
/// same either way, and an empty string is a legitimate way to say "none".
const ASCII_PRINTABLE_OR_EMPTY: &str = r"^[\x20-\x7E]*$";

// One `allowed_subject_types` entry: a GTS Type-id pattern naming an entity kind
// whose events may declare it in `subject_type`. `x-gts-pattern-ref` checks its
// syntax and length (GTS spec §10); `gts.*` places no coverage limit on it.
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct SubjectTypeRef(#[schemars(extend("x-gts-pattern-ref" = gts_id!("*")))] pub String);

/// The default the base declares for [`EventTraits::partition_key`]: the event's
/// tenant. Every event carries one, so the default always resolves.
const TENANT_POINTER: &str = "/tenant_id";

// A JSON Pointer (RFC 6901) into an event, naming the member whose value is
// hashed to pick the partition. `json-pointer` is a registered JSON Schema
// format, so the constraint is one a generic validator can act on.
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("format" = "json-pointer"))]
pub struct PartitionKeyRef(pub String);

// The topic an event type publishes to: an instance of the topic base type, so a
// derived event type naming something that is not a topic is rejected at
// registration rather than at its first publish.
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-instance-ref" = gts_id!("cf.core.events.topic.v1~")))]
pub struct TopicRef(pub String);

// Traits for `gts.cf.core.events.event.v1~`, emitted as the base's
// `x-gts-traits-schema`. They govern how the broker treats events of a type and
// are schema-only keywords, so no publisher can send them and no event payload
// can carry them. A `///` here would become a `description` on the emitted
// trait-schema object; field `///` docs are wanted, since each becomes that
// trait's `description`.
#[allow(clippy::doc_markdown)]
#[derive(Serialize, JsonSchema, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct EventTraits {
    /// Full GTS topic identifier of the stream that events of this type are published to. A topic is an instance, so this never ends in `~`.
    pub topic: TopicRef,
    /// GTS Type-id patterns describing which subject types an event of this type may declare in subject_type (concrete Type match, wildcard suffix `<prefix>.*`, or bare base Type `<base>~` with implicit derived-type coverage). Enforced at publish time, independent of the subject_type:produce/:consume authorization check.
    #[serde(default)]
    pub allowed_subject_types: Vec<SubjectTypeRef>,
    /// JSON Pointer naming the member of an event whose value determines its partition. May point into `data`, which a bare field name could not express. Defaults to the event's tenant, so a type declaring nothing partitions per tenant. The broker checks at registration that the pointer names a member this type's resolved schema declares.
    #[serde(default = "default_partition_key")]
    #[schemars(extend("default" = TENANT_POINTER))]
    pub partition_key: PartitionKeyRef,
}

fn default_partition_key() -> PartitionKeyRef {
    PartitionKeyRef(TENANT_POINTER.to_owned())
}

/// Base topic type. A concrete topic is a well-known instance of it, carrying
/// the stream's own data: what it is called, what it is for, and how long its
/// events are kept. How many partitions the broker gives it and which backend
/// stores them are the broker's concerns, configured there rather than declared
/// per topic.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.events.topic.v1~"),
    description = "Topic resource. A named logical event stream. Sequencing, offsets, and ordering are scoped to a single (topic, partition), and the partition count is broker configuration rather than a property of the topic.",
    properties = "id,description,retention",
)]
pub struct TopicV1 {
    /// Full GTS topic identifier (e.g., gts.cf.core.events.topic.v1~vendor.users.v1).
    #[schemars(
        with = "String",
        extend("x-gts-instance-ref" = SELF_ID_REF)
    )]
    pub id: GtsInstanceId,
    /// What the stream carries, for a reader deciding whether to subscribe to it. ASCII only.
    #[schemars(regex(pattern = ASCII_PRINTABLE_OR_EMPTY), length(max = 1024))]
    pub description: String,
    /// ISO 8601 duration events on this topic are retained for. Absent means the broker's configured default.
    #[serde(default)]
    pub retention: Option<Iso8601Duration>,
}

/// Publish-time transport-metadata block. Stripped on read responses.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct EventMeta {
    /// Meta-block schema version. The broker accepts version <= current_supported and rejects newer with 400 UnknownMetaVersion.
    #[schemars(range(min = 1))]
    pub version: u32,
    /// Producer registered via POST /v1/producers; bound to the calling principal. Required for chained / monotonic mode publishes. Omit for stateless.
    #[serde(default)]
    pub producer_id: Option<Uuid>,
    /// Predecessor's sequence for chain dedup. Required and only valid in chained mode. On the first chained-mode publish for a (producer_id, topic), set to 0.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub previous: Option<i64>,
    /// Producer-assigned monotonic sequence per (producer_id, topic). Required in chained and monotonic modes; omitted in stateless.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub sequence: Option<i64>,
}

/// Base event type. Abstract: every concrete event type is a derived type schema
/// under it, narrowing `data` to its own payload contract and fixing its traits.
///
/// One canonical shape serves both publish input and read responses; direction is
/// encoded per field. `writeOnly` fields are accepted on publish and stripped on
/// read; `readOnly` fields are server-stamped on read and rejected if supplied on
/// publish. `required` is therefore the union of publish-required and
/// read-required, and a strict-validator producer filters `readOnly` fields
/// before submitting.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.events.event.v1~"),
    description = "Event schema. Single canonical resource shape used by both publish input (POST /v1/events, POST /v1/events:batch) and read responses (poll / query). Per-direction semantics are encoded via JSON Schema field-level markers: `writeOnly` fields (meta) are accepted on publish and stripped on read; `readOnly` fields (partition, sequence, sequence_time) are server-stamped on read and rejected with 400 BadRequest if supplied on publish. The top-level `required` list is the union of publish-required and read-required fields; strict-validator producers must filter `readOnly` fields before submission.",
    properties = "id,r#type,tenant_id,source,subject,subject_type,occurred_at,trace_parent,data,partition,sequence,sequence_time,meta",
    traits_schema = inline(EventTraits),
    gts_abstract = true,
)]
pub struct EventV1<P = serde_json::Value> {
    /// Client-provided unique event identifier (UUID).
    id: Uuid,
    /// Full GTS event-type identifier. The event is an instance of that derived type. ASCII only.
    // GTS reference validation enforces identifier syntax and its length limit.
    // `/$id` names the event type being validated, so an event must carry its own
    // type. `with = "String"` preserves that constraint: a field typed
    // `GtsTypeId` is emitted as a `$ref`, and `gts-macros` then replaces the whole
    // property with what the type's `JsonSchema` impl says - an
    // `x-gts-type-ref: "gts.*"` that every Type identifier matches.
    #[schemars(
        with = "String",
        extend("x-gts-type-ref" = SELF_ID_REF)
    )]
    r#type: GtsTypeId,
    /// Tenant the event belongs to. Producer-supplied. Ingest validates the producer's principal is authorized to publish to this tenant via the platform's authz resolver; unauthorized publishes are rejected with 403 TenantIdNotAuthorized.
    tenant_id: Uuid,
    /// Origin of the event (e.g., service name). ASCII only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required, with = "String", regex(pattern = ASCII_PRINTABLE), length(max = 256))]
    source: Option<String>,
    /// Subject entity identifier for event correlation, filtering, and consumer semantics. An event type that needs subject-level ordering points its partition-key trait at this member. ASCII only.
    #[schemars(regex(pattern = ASCII_PRINTABLE), length(min = 1, max = 1024))]
    subject: String,
    /// Full GTS subject-type identifier - the type of the entity the event is about. Required and NOT derivable from `type`: event types may be generic (e.g., `rule_applied`) across multiple subject kinds, and events may have no `data` body to introspect. ASCII only.
    #[schemars(with = "String", extend("x-gts-type-ref" = gts_id!("*")))]
    subject_type: GtsTypeId,
    /// When the event occurred (producer-stamped). ISO 8601 / RFC 3339 timestamp.
    #[schemars(with = "String", extend("format" = "date-time"))]
    occurred_at: DateTime<Utc>,
    /// W3C Trace Context parent. Carries the trace context the event was produced under; surfaced on read responses for consumer-side trace correlation. Validated against the W3C traceparent header format.
    #[serde(default)]
    #[schemars(regex(pattern = r"^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$"))]
    trace_parent: Option<String>,
    /// Event payload, validated at ingest against the resolved schema of the event's type. The only field where UTF-8 (or any non-ASCII bytes) is permitted; all other event fields are ASCII per platform convention. May be absent for body-less events (e.g., notification-only events whose semantics are fully carried by `type` + `subject`).
    data: Option<P>,
    /// Server-stamped partition for the event, surfaced on read. Computed at ingest via the partition contract. Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required, with = "i32", range(min = 0), extend("readOnly" = true))]
    partition: Option<u32>,
    /// Server-assigned monotonic ordering key per (topic, partition), surfaced on read. The only sequence consumers paginate by. Distinct from `meta.sequence` (publish-only producer-side chain field). Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required, with = "i64", range(min = 0), extend("readOnly" = true))]
    sequence: Option<Sequence>,
    /// Server-stamped timestamp recording when `sequence` was assigned. Surfaced on read. Producers MUST NOT supply this field on publish; doing so is rejected with 400 BadRequest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        required,
        with = "String",
        extend("readOnly" = true, "format" = "date-time")
    )]
    sequence_time: Option<DateTime<Utc>>,
    /// Optional publish-time transport-metadata block. Carries producer-protocol fields (producer_id, previous, sequence) for chained / monotonic modes. Omit entirely for stateless publish. Stripped on read responses; consumers MUST NOT rely on it.
    #[serde(default)]
    #[schemars(extend("writeOnly" = true))]
    meta: Option<EventMeta>,
}

/// Any payload type a derived event type declares: a macro-declared derived struct,
/// `serde_json::Value` (untyped), or a serde type with a hand-written `impl gts::GtsSchema`.
///
/// A payload declared with `#[gts_type_schema]` writes and reads every field as present: an
/// unset `Option` field is written as `null`, and a missing one fails to decode (the macro
/// applies neither `skip_serializing_if` nor `serde(default)`). Give a scalar `Option` field
/// `#[schemars(required, extend("type" = [<its JSON type>, "null"]))]`, so the registered
/// schema requires it and admits `null` and the broker refuses an event that omits it. The
/// same attribute, with `"object"`, covers a structured `Option` field.
pub trait EventPayload:
    GtsSchema + ::gts::GtsSerialize + for<'de> ::gts::GtsDeserialize<'de> + Send + Sync + 'static
{
}

impl<T> EventPayload for T where
    T: GtsSchema
        + ::gts::GtsSerialize
        + for<'de> ::gts::GtsDeserialize<'de>
        + Send
        + Sync
        + 'static
{
}

// `P: GtsSchema` is the bound `#[gts_type_schema]` puts on the struct's parameter.
impl<P: GtsSchema> EventV1<P> {
    /// The event's own type id: the payload's `GtsSchema::TYPE_ID`. Read this, not
    /// `TYPE_ID`: on `EventV1<P>` (or an alias of it) `TYPE_ID` is the base type's id,
    /// `gts.cf.core.events.event.v1~`. It is empty for `P = serde_json::Value`, whose
    /// events carry their type in `event_type()`, and for a nested macro chain it is
    /// the direct child's id.
    pub const EVENT_TYPE_ID: &'static str = <P as GtsSchema>::TYPE_ID;

    /// Client-provided unique event identifier.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The event's own GTS event-type identifier.
    #[must_use]
    pub fn event_type(&self) -> &GtsTypeId {
        &self.r#type
    }

    /// Tenant the event belongs to.
    #[must_use]
    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    /// Origin of the event; `None` until it is set. A stored or delivered event always
    /// carries one.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// Subject entity identifier.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// GTS type of the entity the event is about.
    #[must_use]
    pub fn subject_type(&self) -> &GtsTypeId {
        &self.subject_type
    }

    /// When the event occurred, as the producer stamped it.
    #[must_use]
    pub fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }

    /// W3C Trace Context parent the event was produced under.
    #[must_use]
    pub fn trace_parent(&self) -> Option<&str> {
        self.trace_parent.as_deref()
    }

    /// Event payload; `None` for a body-less event.
    #[must_use]
    pub fn data(&self) -> Option<&P> {
        self.data.as_ref()
    }

    /// Consumes the event and returns its payload.
    #[must_use]
    pub fn into_data(self) -> Option<P> {
        self.data
    }

    /// Server-stamped partition; `None` until the event is stored.
    #[must_use]
    pub fn partition(&self) -> Option<u32> {
        self.partition
    }

    /// Server-assigned sequence; `None` until the event is stored.
    #[must_use]
    pub fn sequence(&self) -> Option<Sequence> {
        self.sequence
    }

    /// When `sequence` was assigned; `None` until the event is stored.
    #[must_use]
    pub fn sequence_time(&self) -> Option<DateTime<Utc>> {
        self.sequence_time
    }

    /// Publish-time transport metadata, if the producer supplied it.
    #[must_use]
    pub fn meta(&self) -> Option<&EventMeta> {
        self.meta.as_ref()
    }

    /// Checks that a transport may send this event: a transport calls it before it
    /// publishes. The event carries no broker-stamped member and carries a source.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`], for the first failure in this order:
    /// - `partition`, `sequence` or `sequence_time` when it is set;
    /// - `source` when it is unset.
    pub fn check_publishable(&self) -> Result<(), EventBrokerError> {
        self.check_unstamped()?;
        if self.source.is_none() {
            return Err(EventBrokerError::InvalidEventField {
                field: "source",
                detail: "required".to_owned(),
            });
        }
        Ok(())
    }

    /// Checks that a producer may take this event as input: a producer calls it before
    /// it stamps the event. The event carries no broker-stamped member, no `meta` and no
    /// source, since the producer sets the last two itself.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`], for the first failure in this order:
    /// - `partition`, `sequence` or `sequence_time` when it is set;
    /// - `meta` when it is set;
    /// - `source` when it is set.
    pub fn check_producer_input(&self) -> Result<(), EventBrokerError> {
        self.check_unstamped()?;
        if self.meta.is_some() {
            return Err(Self::producer_stamped("meta"));
        }
        if self.source.is_some() {
            return Err(Self::producer_stamped("source"));
        }
        Ok(())
    }

    fn check_unstamped(&self) -> Result<(), EventBrokerError> {
        let read_only = |field| EventBrokerError::InvalidEventField {
            field,
            detail: "broker-stamped and read-only on publish".to_owned(),
        };
        if self.partition.is_some() {
            return Err(read_only("partition"));
        }
        if self.sequence.is_some() {
            return Err(read_only("sequence"));
        }
        if self.sequence_time.is_some() {
            return Err(read_only("sequence_time"));
        }
        Ok(())
    }

    fn producer_stamped(field: &'static str) -> EventBrokerError {
        EventBrokerError::InvalidEventField {
            field,
            detail: "the producer stamps it".to_owned(),
        }
    }

    /// Checks the envelope's values that a decoded event can break (`source`, `subject`
    /// and `trace_parent`) against the rules [`EventV1Builder::build`] applies, with the
    /// same errors: a producer calls it before it sends an event, since an event read with
    /// serde never passed through the builder. `type` and `subject_type` are not
    /// re-checked: the builder refuses an empty one, and decoding a `GtsTypeId` refuses an
    /// empty id, so neither is empty on a built or decoded event. Presence is not checked
    /// either: a built or decoded `EventV1` always has a `tenant_id`, a `subject` and a
    /// `subject_type`.
    ///
    /// # Errors
    ///
    /// Returns the first failure, in this order:
    /// - [`EventBrokerError::InvalidTextField`] for `source` when it is set and breaks
    ///   [`crate::validate::source`];
    /// - [`EventBrokerError::InvalidTextField`] for `subject` when it breaks
    ///   [`crate::validate::subject`];
    /// - [`EventBrokerError::InvalidEventField`] for `trace_parent` when it is set and is
    ///   not a W3C traceparent.
    pub(crate) fn check_envelope(&self) -> Result<(), EventBrokerError> {
        if let Some(source) = &self.source {
            Self::check_source(source)?;
        }
        Self::check_subject(&self.subject)?;
        if let Some(trace_parent) = &self.trace_parent {
            Self::check_trace_parent(trace_parent)?;
        }
        Ok(())
    }

    fn check_type(r#type: &GtsTypeId) -> Result<(), EventBrokerError> {
        if r#type.is_empty() {
            return Err(Self::required("type"));
        }
        Ok(())
    }

    fn check_source(source: &str) -> Result<(), EventBrokerError> {
        crate::validate::source(source).map_err(|err| EventBrokerError::invalid_text_field(&err))
    }

    fn check_subject(subject: &str) -> Result<(), EventBrokerError> {
        crate::validate::subject(subject).map_err(|err| EventBrokerError::invalid_text_field(&err))
    }

    fn check_subject_type(subject_type: &GtsTypeId) -> Result<(), EventBrokerError> {
        if subject_type.is_empty() {
            return Err(Self::required("subject_type"));
        }
        Ok(())
    }

    /// Refuses a `trace_parent` that is not a W3C traceparent.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`] for `trace_parent`.
    pub(crate) fn check_trace_parent(trace_parent: &str) -> Result<(), EventBrokerError> {
        if !crate::validate::is_trace_parent(trace_parent) {
            return Err(EventBrokerError::InvalidEventField {
                field: "trace_parent",
                detail: crate::validate::TRACE_PARENT_RULE.to_owned(),
            });
        }
        Ok(())
    }

    fn required(field: &'static str) -> EventBrokerError {
        EventBrokerError::InvalidEventField {
            field,
            detail: "required".to_owned(),
        }
    }
}

// `P: GtsSchema` is the bound `#[gts_type_schema]` puts on the struct's parameter.
impl<P: GtsSchema + Clone> Clone for EventV1<P> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            r#type: self.r#type.clone(),
            tenant_id: self.tenant_id,
            source: self.source.clone(),
            subject: self.subject.clone(),
            subject_type: self.subject_type.clone(),
            occurred_at: self.occurred_at,
            trace_parent: self.trace_parent.clone(),
            data: self.data.clone(),
            partition: self.partition,
            sequence: self.sequence,
            sequence_time: self.sequence_time,
            meta: self.meta.clone(),
        }
    }
}

// Prints the payload's type name instead of the payload: payloads may carry
// personal or secret data, and `P` is not required to implement `Debug`.
impl<P: GtsSchema> std::fmt::Debug for EventV1<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventV1")
            .field("id", &self.id)
            .field("type", &self.r#type)
            .field("tenant_id", &self.tenant_id)
            .field("source", &self.source)
            .field("subject", &self.subject)
            .field("subject_type", &self.subject_type)
            .field("occurred_at", &self.occurred_at)
            .field("trace_parent", &self.trace_parent)
            .field(
                "data",
                &self.data.as_ref().map(|_| std::any::type_name::<P>()),
            )
            .field("partition", &self.partition)
            .field("sequence", &self.sequence)
            .field("sequence_time", &self.sequence_time)
            .field("meta", &self.meta)
            .finish()
    }
}

impl<P: EventPayload> EventV1<P> {
    /// Starts an event carrying `payload`; its `type` is the payload type's id.
    #[must_use]
    pub fn builder(payload: P) -> EventV1Builder<P> {
        EventV1Builder {
            data: Some(payload),
            ..EventV1Builder::new(GtsTypeId::new(P::innermost_type_id()))
        }
    }
}

/// The raw form of a typed event: `data` serialized as JSON, the envelope fields copied.
impl<P: EventPayload> TryFrom<&EventV1<P>> for EventV1 {
    type Error = EventBrokerError;

    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when `data` does not serialize, which names only
    /// the serde_json failure class.
    fn try_from(event: &EventV1<P>) -> Result<Self, EventBrokerError> {
        let data = event
            .data
            .as_ref()
            .map(|payload| serde_json::to_value(::gts::GtsSerializeWrapper(payload)))
            .transpose()
            .map_err(|err| {
                EventBrokerError::Internal(format!(
                    "serialize event data ({})",
                    crate::error::json_error_category(&err)
                ))
            })?;
        Ok(EventV1 {
            id: event.id,
            r#type: event.r#type.clone(),
            tenant_id: event.tenant_id,
            source: event.source.clone(),
            subject: event.subject.clone(),
            subject_type: event.subject_type.clone(),
            occurred_at: event.occurred_at,
            trace_parent: event.trace_parent.clone(),
            data,
            partition: event.partition,
            sequence: event.sequence,
            sequence_time: event.sequence_time,
            meta: event.meta.clone(),
        })
    }
}

/// Where a stored event sits: what the broker stamps on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredPosition {
    /// The partition the event is stored in.
    pub partition: u32,
    /// The event's sequence within that partition.
    pub sequence: Sequence,
    /// When the sequence was assigned.
    pub sequence_time: DateTime<Utc>,
}

impl EventV1 {
    /// Starts an untyped event of type `type_id` carrying `data`.
    #[must_use]
    pub fn builder_for(
        type_id: GtsTypeId,
        data: serde_json::Value,
    ) -> EventV1Builder<serde_json::Value> {
        EventV1Builder {
            data: Some(data),
            ..EventV1Builder::new(type_id)
        }
    }

    /// Starts an untyped, body-less event of type `type_id`.
    #[must_use]
    pub fn builder_without_data(type_id: GtsTypeId) -> EventV1Builder<serde_json::Value> {
        EventV1Builder::new(type_id)
    }

    /// Typed view of this event: its `data` decoded as `P`, its envelope fields copied. The
    /// raw event stays with the caller. The event's `type` is not compared with `P`;
    /// choosing `P` for a type is the caller's routing.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Decode`] when `data` does not fit `P`. It names the type and the
    /// failure class and carries no part of the data.
    pub fn decode<P: EventPayload>(&self) -> Result<EventV1<P>, EventBrokerError> {
        // The serde_json error is dropped unformatted: its message can quote the data.
        let data = self
            .data
            .as_ref()
            .map(<P as ::gts::GtsDeserialize<'_>>::gts_deserialize)
            .transpose()
            .map_err(|err| EventBrokerError::Decode {
                type_id: self.r#type.to_string(),
                category: err.classify().into(),
            })?;
        Ok(EventV1 {
            id: self.id,
            r#type: self.r#type.clone(),
            tenant_id: self.tenant_id,
            source: self.source.clone(),
            subject: self.subject.clone(),
            subject_type: self.subject_type.clone(),
            occurred_at: self.occurred_at,
            trace_parent: self.trace_parent.clone(),
            data,
            partition: self.partition,
            sequence: self.sequence,
            sequence_time: self.sequence_time,
            meta: self.meta.clone(),
        })
    }

    /// Sets the three broker-stamped members from `at`.
    #[must_use]
    pub fn stamp_position(self, at: StoredPosition) -> Self {
        Self {
            partition: Some(at.partition),
            sequence: Some(at.sequence),
            sequence_time: Some(at.sequence_time),
            ..self
        }
    }

    /// Sets the producer metadata; only a producer stamps it.
    pub(crate) fn stamp_meta(&mut self, meta: EventMeta) {
        self.meta = Some(meta);
    }

    /// Sets the source; only a producer stamps it, from its own.
    pub(crate) fn stamp_source(&mut self, source: String) {
        self.source = Some(source);
    }

    /// Resolves an event type's partition-key JSON Pointer against this event, as
    /// [`crate::models::Event::partition_input`] does for the backend record.
    ///
    /// # Errors
    ///
    /// As [`crate::models::resolve_partition_input`].
    pub(crate) fn partition_input(&self, pointer: &str) -> Result<String, EventBrokerError> {
        crate::models::resolve_partition_input(&self.addressable(), pointer)
    }

    /// The event's publish-time members by their schema names: the nine members
    /// [`crate::models::Event`] exposes to a partition-key pointer.
    fn addressable(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "type": self.r#type,
            "tenant_id": self.tenant_id,
            "source": self.source,
            "subject": self.subject,
            "subject_type": self.subject_type,
            "occurred_at": self.occurred_at,
            "trace_parent": self.trace_parent,
            "data": self.data,
        })
    }
}

/// Builds an [`EventV1`]; [`build`](Self::build) checks the envelope fields, not `data`.
// Holds its own fields rather than an `EventV1<P>`, so the struct and its `Debug`
// need no `P: GtsSchema` bound.
pub struct EventV1Builder<P> {
    r#type: GtsTypeId,
    data: Option<P>,
    id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    source: Option<String>,
    subject: Option<String>,
    subject_type: Option<GtsTypeId>,
    occurred_at: Option<DateTime<Utc>>,
    trace_parent: Option<String>,
}

impl<P: EventPayload> EventV1Builder<P> {
    fn new(r#type: GtsTypeId) -> Self {
        Self {
            r#type,
            data: None,
            id: None,
            tenant_id: None,
            source: None,
            subject: None,
            subject_type: None,
            occurred_at: None,
            trace_parent: None,
        }
    }

    /// Sets the event id; without it, `build()` mints a UUID v7.
    #[must_use]
    pub fn id(mut self, id: Uuid) -> Self {
        self.id = Some(id);
        self
    }

    /// Sets the tenant the event belongs to. Required.
    #[must_use]
    pub fn tenant_id(mut self, id: Uuid) -> Self {
        self.tenant_id = Some(id);
        self
    }

    /// Sets the origin of the event.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Sets the subject entity identifier. Required.
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Sets the subject type to the declared kind `S`. Replaces any earlier subject type.
    #[must_use]
    pub fn subject_type<S: GtsSchema>(mut self) -> Self {
        self.subject_type = Some(GtsTypeId::new(S::TYPE_ID));
        self
    }

    /// Sets the subject type by id, for a kind not declared in Rust. Replaces any earlier
    /// subject type.
    #[must_use]
    pub fn subject_type_id(mut self, subject_type: GtsTypeId) -> Self {
        self.subject_type = Some(subject_type);
        self
    }

    /// Sets when the event occurred; without it, `build()` takes the current time.
    #[must_use]
    pub fn occurred_at(mut self, at: DateTime<Utc>) -> Self {
        self.occurred_at = Some(at);
        self
    }

    /// Sets the W3C Trace Context parent the event is produced under.
    #[must_use]
    pub fn trace_parent(mut self, trace_parent: impl Into<String>) -> Self {
        self.trace_parent = Some(trace_parent.into());
        self
    }

    /// Checks the envelope fields and returns the event. An unset `id` becomes a new UUID v7
    /// and an unset `occurred_at` the current time, both fixed here.
    ///
    /// # Errors
    ///
    /// Returns the first failure, in this order:
    /// - [`EventBrokerError::InvalidEventField`] for `type` when it is empty, as for an
    ///   untyped payload passed to [`EventV1::builder`];
    /// - [`EventBrokerError::InvalidEventField`] for `tenant_id` when it is unset;
    /// - [`EventBrokerError::InvalidTextField`] for `source` when it is set and breaks
    ///   [`crate::validate::source`];
    /// - [`EventBrokerError::InvalidEventField`] for `subject` when it is unset, or
    ///   [`EventBrokerError::InvalidTextField`] when it breaks [`crate::validate::subject`];
    /// - [`EventBrokerError::InvalidEventField`] for `subject_type` when it is unset or
    ///   empty;
    /// - [`EventBrokerError::InvalidEventField`] for `trace_parent` when it is set and is
    ///   not a W3C traceparent.
    pub fn build(self) -> Result<EventV1<P>, EventBrokerError> {
        // The value rules are `EventV1`'s; `check_envelope` shares those a decoded
        // event can break.
        EventV1::<P>::check_type(&self.r#type)?;
        let Some(tenant_id) = self.tenant_id else {
            return Err(EventV1::<P>::required("tenant_id"));
        };
        if let Some(source) = &self.source {
            EventV1::<P>::check_source(source)?;
        }
        let Some(subject) = self.subject else {
            return Err(EventV1::<P>::required("subject"));
        };
        EventV1::<P>::check_subject(&subject)?;
        let Some(subject_type) = self.subject_type else {
            return Err(EventV1::<P>::required("subject_type"));
        };
        EventV1::<P>::check_subject_type(&subject_type)?;
        if let Some(trace_parent) = &self.trace_parent {
            EventV1::<P>::check_trace_parent(trace_parent)?;
        }

        Ok(EventV1 {
            id: self.id.unwrap_or_else(Uuid::now_v7),
            r#type: self.r#type,
            tenant_id,
            source: self.source,
            subject,
            subject_type,
            occurred_at: self.occurred_at.unwrap_or_else(Utc::now),
            trace_parent: self.trace_parent,
            data: self.data,
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        })
    }
}

// Prints the payload's type name instead of the payload, as `EventV1`'s `Debug` does.
impl<P> std::fmt::Debug for EventV1Builder<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventV1Builder")
            .field("type", &self.r#type)
            .field(
                "data",
                &self.data.as_ref().map(|_| std::any::type_name::<P>()),
            )
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("source", &self.source)
            .field("subject", &self.subject)
            .field("subject_type", &self.subject_type)
            .field("occurred_at", &self.occurred_at)
            .field("trace_parent", &self.trace_parent)
            .finish()
    }
}

/// Base subject type. A subject is the entity an event is about; an event names
/// its kind in `subject_type`. Abstract: a gear declares each subject kind it owns
/// as a derived type schema under it. `EventV1`'s `subject_type` accepts any GTS
/// type id, so this is a convention, not a requirement.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.events.subject.v1~"),
    description = "Subject type. The kind of entity an event is about; an event names it in `subject_type`. By convention, a gear declares each subject kind it owns as a derived type schema under this base; `subject_type` accepts any GTS type id.",
    properties = "r#type,id,properties",
    gts_abstract = true,
)]
pub struct SubjectV1<P: GtsSchema> {
    /// Full GTS subject-type identifier: the derived type this subject is an instance of.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
    /// The subject's identifier, as events carry it in `subject`. ASCII only.
    #[schemars(regex(pattern = ASCII_PRINTABLE), length(min = 1, max = 1024))]
    pub id: String,
    /// Kind-specific attributes declared by the derived subject type.
    pub properties: P,
}

// Prints the properties' type name: a kind's attributes may carry entity data, and
// `P` is not required to implement `Debug`.
impl<P: GtsSchema> std::fmt::Debug for SubjectV1<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubjectV1")
            .field("type", &self.r#type)
            .field("id", &self.id)
            .field("properties", &std::any::type_name::<P>())
            .finish()
    }
}

/// Every `data` constraint a resolved event-type schema declares, composed into
/// one subschema.
///
/// A resolved event-type schema describes a whole event, and the base marks the
/// server-stamped members both `required` and `readOnly`, so validating a publish
/// payload against it directly would fail on fields the producer is forbidden to
/// send. Producer-side validation therefore targets the payload contract alone,
/// which is what each type in the chain narrows.
///
/// Resolution inlines each ancestor into its descendant's `allOf`, so a chain
/// deeper than two nests `allOf` within `allOf`. The walk recurses for that
/// reason: a flat pass over the immediate branches would silently drop the
/// constraints of every type above the parent. Branches are visited before a
/// node's own `properties.data`, so ancestors compose ahead of descendants and
/// the narrowest constraint lands last.
#[must_use]
pub fn data_contract(resolved: &serde_json::Value) -> serde_json::Value {
    /// Bounds the walk so a schema that resolved into a cycle cannot recurse
    /// forever. Real chains are two or three deep.
    const MAX_DEPTH: usize = 32;

    fn collect(node: &serde_json::Value, depth: usize, out: &mut Vec<serde_json::Value>) {
        if depth == MAX_DEPTH {
            return;
        }
        if let Some(branches) = node.get("allOf").and_then(serde_json::Value::as_array) {
            for branch in branches {
                collect(branch, depth + 1, out);
            }
        }
        if let Some(data) = node.get("properties").and_then(|props| props.get("data")) {
            out.push(data.clone());
        }
    }

    let mut branches = Vec::new();
    collect(resolved, 0, &mut branches);
    serde_json::json!({ "allOf": branches })
}

/// Assembles the derived event-type schema document for `type_id`: the payload
/// contract as a narrowing of the base's `data` member, and the governing
/// metadata as `x-gts-traits`.
///
/// Test fixtures and the mock register event types through this rather than
/// storing a bare payload schema, so what they hold is the same shape
/// `types-registry` provisions - which means the shape it will actually admit.
///
/// A `data_schema` with no constraints of its own is rendered as the base's own
/// `["object", "null"]` rather than passed through as an empty schema. An empty
/// schema admits any JSON value, which is *wider* than the base's member, and a
/// derived type may only narrow: registration refuses the document with
/// "Schema at '$.data' changes type incompatibly". A caller stating no
/// constraints means "whatever the base allows", and that is what this writes.
#[must_use]
pub fn derived_event_type_schema(
    type_id: &str,
    topic: &str,
    data_schema: serde_json::Value,
    allowed_subject_types: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("{}{type_id}", toolkit_gts::GTS_ID_URI_PREFIX),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic,
            "allowed_subject_types": allowed_subject_types,
            "partition_key": TENANT_POINTER,
        },
        "type": "object",
        "allOf": [
            { "$ref": toolkit_gts::gts_uri!("cf.core.events.event.v1~") },
            {
                "type": "object",
                "properties": {
                    "data": if data_schema
                        .as_object()
                        .is_some_and(serde_json::Map::is_empty)
                    {
                        serde_json::json!({ "type": ["object", "null"] })
                    } else {
                        data_schema
                    },
                },
            },
        ],
    })
}

/// Consumer-group resource type - PEP/error matching only, no schema
/// generation (no `types-registry` inventory entry).
pub const CONSUMER_GROUP_RESOURCE_TYPE: &str = gts_id!("cf.core.events.consumer_group.v1~");
/// Legacy event-type id used only in error payloads; not registered in types-registry.
/// Authorization uses concrete event types or the `EventV1` base type for wildcard
/// interests (`docs/DESIGN.md` §3.3).
pub const EVENT_TYPE_RESOURCE_TYPE: &str = gts_id!("cf.core.events.event_type.v1~");
/// Subscription resource type - PEP/error matching only.
pub const SUBSCRIPTION_RESOURCE_TYPE: &str = gts_id!("cf.core.events.subscription.v1~");
/// Producer resource type - PEP/error matching only.
pub const PRODUCER_RESOURCE_TYPE: &str = gts_id!("cf.core.events.producer.v1~");
/// Generic fallback resource type for errors not tied to one entity kind
/// (mirrors `api/rest/error.rs::EventBrokerResourceError`'s own fallback).
pub const REQUEST_RESOURCE_TYPE: &str = gts_id!("cf.core.events.request.v1~");
