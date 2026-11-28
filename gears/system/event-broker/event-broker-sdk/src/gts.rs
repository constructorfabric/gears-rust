//! GTS type definitions the broker owns.
//!
//! Two base types are declared here, and the broker declares nothing else. A
//! concrete topic is a well-known **instance** of [`TopicV1`]; a concrete event
//! type is a **derived type schema** under [`EventV1`], and a published event is
//! an instance of that derived type. The gear that owns a concrete topic or
//! event type registers it in `types-registry`. The producer chooses the
//! event's `subject_type`, typically its own domain type such as
//! `gts.acme.crm.accounts.tenant.v1~`.
//!
//! The broker publishes these base schemas from the SDK's crate-local
//! inventory. Embedded registries also see them through the process-wide
//! inventory. The base files under `docs/schemas/` are generated from this
//! module and compared with its emitted schemas by `gts_tests`.
//! Gears declare event types with `#[gts_type_schema(base = EventV1, ..)]`;
//! the derived struct's fields define the event's `data` contract.

use chrono::{DateTime, Utc};
use gts::{GtsInstanceId, GtsSchema, GtsTypeId, SELF_ID_REF};
use schemars::JsonSchema;
use serde::Serialize;
use toolkit_gts::local::gts_type_schema;
use toolkit_gts::{GtsTraitsSchema, gts_id};
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

/// The default the base declares for [`EventV1Traits::partition_key`]: the event's
/// tenant. Every event carries one, so the default always resolves.
const SUBJECT_TENANT_POINTER: &str = "/subject_tenant";

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
pub struct EventV1Traits {
    /// Full GTS topic identifier of the stream that events of this type are published to. A topic is an instance, so this never ends in `~`.
    topic: TopicRef,
    /// GTS Type-id patterns describing which subject types an event of this type may declare in subject_type (concrete Type match, wildcard suffix `<prefix>.*`, or bare base Type `<base>~` with implicit derived-type coverage). Enforced at publish time, independent of the subject_type:produce/:consume authorization check.
    #[serde(default)]
    allowed_subject_types: Vec<SubjectTypeRef>,
    /// JSON Pointer naming the member of an event whose value determines its partition. May point into `data`, which a bare field name could not express. Defaults to the event's tenant, so a type declaring nothing partitions per tenant. The broker checks at registration that the pointer names a member this type's resolved schema declares.
    #[serde(default = "default_partition_key")]
    #[schemars(extend("default" = SUBJECT_TENANT_POINTER))]
    partition_key: PartitionKeyRef,
}

impl EventV1Traits {
    /// Starts a builder requiring a topic and allowed subject-type patterns.
    ///
    /// Both required methods may be called in either order. The partition
    /// pointer defaults to `/subject_tenant`. All values are serialized explicitly.
    #[must_use]
    pub fn builder() -> EventV1TraitsBuilder {
        EventV1TraitsBuilder {
            topic: (),
            allowed_subject_types: (),
            partition_key: default_partition_key(),
        }
    }
}

/// Builds [`EventV1Traits`]; `build` is available only after both required setters.
///
/// The type parameters store each required value, or `()` while it is missing.
pub struct EventV1TraitsBuilder<T = (), S = ()> {
    topic: T,
    allowed_subject_types: S,
    partition_key: PartitionKeyRef,
}

impl<T, S> EventV1TraitsBuilder<T, S> {
    /// Sets the topic that events of this type are published to (required).
    #[must_use]
    pub fn with_topic(self, topic: impl Into<String>) -> EventV1TraitsBuilder<TopicRef, S> {
        EventV1TraitsBuilder {
            topic: TopicRef(topic.into()),
            allowed_subject_types: self.allowed_subject_types,
            partition_key: self.partition_key,
        }
    }

    /// Sets the allowed subject-type patterns (required).
    ///
    /// An explicitly empty list denies every subject type at publish time.
    #[must_use]
    pub fn with_allowed_subject_types(
        self,
        patterns: impl IntoIterator<Item = impl Into<String>>,
    ) -> EventV1TraitsBuilder<T, Vec<SubjectTypeRef>> {
        EventV1TraitsBuilder {
            topic: self.topic,
            allowed_subject_types: patterns
                .into_iter()
                .map(|pattern| SubjectTypeRef(pattern.into()))
                .collect(),
            partition_key: self.partition_key,
        }
    }

    /// Sets the JSON Pointer used to partition events, overriding `/subject_tenant`.
    #[must_use]
    pub fn with_partition_key(mut self, pointer: impl Into<String>) -> Self {
        self.partition_key = PartitionKeyRef(pointer.into());
        self
    }
}

impl EventV1TraitsBuilder<TopicRef, Vec<SubjectTypeRef>> {
    /// Builds the traits after a topic and allowed subject types have been supplied.
    #[must_use]
    pub fn build(self) -> EventV1Traits {
        EventV1Traits {
            topic: self.topic,
            allowed_subject_types: self.allowed_subject_types,
            partition_key: self.partition_key,
        }
    }
}

fn default_partition_key() -> PartitionKeyRef {
    PartitionKeyRef(SUBJECT_TENANT_POINTER.to_owned())
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
    /// Meta-block schema version. The broker supports version 1 and rejects a higher one with `400 UnknownMetaVersion`; it does not enforce the minimum declared here, so a lower value is accepted.
    #[schemars(range(min = 1))]
    pub version: u32,
    /// Producer registered via POST /v1/producers; bound to the calling principal. Required for chained / monotonic mode publishes. Omit for stateless.
    #[serde(default)]
    pub producer: Option<Uuid>,
    /// Predecessor's sequence for chain dedup. Required and only valid in chained mode. On the first chained-mode publish for a (producer, topic), set to 0.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub previous: Option<i64>,
    /// Producer-assigned monotonic sequence per (producer, topic). Required in chained and monotonic modes; omitted in stateless.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub sequence: Option<i64>,
}

impl EventMeta {
    /// The longest `meta` a producer stamps: a producer id, and both sequences at their
    /// widest value.
    fn largest_stamp() -> Self {
        Self {
            version: 1,
            producer: Some(Uuid::nil()),
            previous: Some(i64::MAX),
            sequence: Some(i64::MAX),
        }
    }
}

/// Base event type. Abstract: every concrete event type is a derived type schema
/// under it, narrowing `data` to its own payload contract and fixing its traits.
///
/// One canonical shape serves both publish input and read responses; direction is
/// encoded per field. `writeOnly` fields are accepted on publish and stripped on read;
/// `readOnly` fields are server-stamped on read and rejected if supplied on publish.
/// `required` is therefore the union of publish-required and read-required, and a
/// strict-validator producer filters `readOnly` fields before submitting.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.events.event.v1~"),
    description = "Event schema. Single canonical resource shape used by both publish input (POST /v1/events, POST /v1/events:batch) and read responses (the `payload` of each `event` frame of GET /v1/events:stream and GET /v1/events:sse). Per-direction semantics are encoded via JSON Schema field-level markers: `writeOnly` fields (meta) are accepted on publish and stripped on read; `readOnly` fields (partition, sequence, sequence_time) are server-stamped on read and rejected with 400 InvalidBody if supplied on publish. The top-level `required` list is the union of publish-required and read-required fields; strict-validator producers must filter `readOnly` fields before submission.",
    properties = "id,r#type,subject_tenant,source,subject,subject_type,subject_owner,occurred_at,trace_parent,data,partition,sequence,sequence_time,meta",
    traits_schema = inline(EventV1Traits),
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
    /// The tenant of the subject the event is about. Producer-supplied. A nil UUID is refused with 400 InvalidTenant; the publishing principal must reach this tenant, and to reach it across a self-managed tenant must also hold produce_ignore_barrier on the event type; otherwise the publish is refused with 403 TenantIdNotAuthorized.
    subject_tenant: Uuid,
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
    /// The platform principal of the subject's tenant that owns the event, or null for a tenant-wide event. Producer-supplied and optional: an explicit null is the same as an absent member, and a nil UUID is refused with 400 InvalidSubjectOwner. An event type whose partition key is `/subject_owner` must narrow it to a required UUID string.
    #[schemars(required, extend("type" = ["string", "null"], "format" = "uuid"))]
    subject_owner: Option<Uuid>,
    /// When the event occurred (producer-stamped). ISO 8601 / RFC 3339 timestamp.
    #[schemars(with = "String", extend("format" = "date-time"))]
    occurred_at: DateTime<Utc>,
    /// W3C Trace Context parent. Carries the trace context the event was produced under; surfaced on read responses for consumer-side trace correlation. Validated against the W3C traceparent header format.
    #[serde(default)]
    #[schemars(regex(pattern = r"^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$"))]
    trace_parent: Option<String>,
    /// Event payload, validated at ingest against the resolved schema of the event's type. The only field where UTF-8 (or any non-ASCII bytes) is permitted; all other event fields are ASCII per platform convention. May be absent for body-less events (e.g., notification-only events whose semantics are fully carried by `type` + `subject`).
    data: Option<P>,
    /// Server-stamped partition for the event, surfaced on read. Assigned by the storage backend from the key the event type's `partition_key` trait resolves to. Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required, with = "i32", range(min = 0), extend("readOnly" = true))]
    partition: Option<u32>,
    /// Server-assigned monotonic ordering key per (topic, partition), surfaced on read. Consumer offsets (an exact SEEK position and a committed cursor) are values of it. Distinct from `meta.sequence` (publish-only producer-side chain field). Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(required, with = "i64", range(min = 0), extend("readOnly" = true))]
    sequence: Option<Sequence>,
    /// Server-stamped timestamp recording when `sequence` was assigned. Surfaced on read. Producers MUST NOT supply this field on publish; doing so is rejected with 400 InvalidBody.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        required,
        with = "String",
        extend("readOnly" = true, "format" = "date-time")
    )]
    sequence_time: Option<DateTime<Utc>>,
    /// Optional publish-time transport-metadata block. Carries producer-protocol fields (producer, previous, sequence) for chained / monotonic modes. Omit entirely for stateless publish. Stripped on read responses; consumers MUST NOT rely on it.
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
    pub const EVENT_TYPE_ID: &str = <P as GtsSchema>::TYPE_ID;

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

    /// The tenant of the subject the event is about.
    #[must_use]
    pub fn subject_tenant(&self) -> Uuid {
        self.subject_tenant
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

    /// The principal that owns the event; `None` for a tenant-wide event.
    #[must_use]
    pub fn subject_owner(&self) -> Option<Uuid> {
        self.subject_owner
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

    /// Checks the envelope's values that a decoded event can break (`source`, `subject`,
    /// `subject_owner` and `trace_parent`) against the rules [`EventV1Builder::build`]
    /// applies, with the same errors: a producer calls it before it sends an event, since
    /// an event read with serde never passed through the builder. `type` and `subject_type` are not
    /// re-checked: the builder refuses an empty one, and decoding a `GtsTypeId` refuses an
    /// empty id, so neither is empty on a built or decoded event. Presence is not checked
    /// either: a built or decoded `EventV1` always has a `subject_tenant`, a `subject` and a
    /// `subject_type`.
    ///
    /// # Errors
    ///
    /// Returns the first failure, in this order:
    /// - [`EventBrokerError::InvalidTextField`] for `source` when it is set and breaks
    ///   [`crate::validate::source`];
    /// - [`EventBrokerError::InvalidTextField`] for `subject` when it breaks
    ///   [`crate::validate::subject`];
    /// - [`EventBrokerError::InvalidEventField`] for `subject_owner` when it is the nil
    ///   UUID;
    /// - [`EventBrokerError::InvalidEventField`] for `trace_parent` when it is set and is
    ///   not a W3C traceparent.
    pub(crate) fn check_envelope(&self) -> Result<(), EventBrokerError> {
        if let Some(source) = &self.source {
            Self::check_source(source)?;
        }
        Self::check_subject(&self.subject)?;
        if let Some(owner) = self.subject_owner {
            Self::check_subject_owner(owner)?;
        }
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

    /// Refuses the nil UUID as an owner: absence is how an event says it has none.
    fn check_subject_owner(owner: Uuid) -> Result<(), EventBrokerError> {
        if owner.is_nil() {
            return Err(EventBrokerError::InvalidEventField {
                field: "subject_owner",
                detail: "must not be the nil UUID".to_owned(),
            });
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
            subject_tenant: self.subject_tenant,
            source: self.source.clone(),
            subject: self.subject.clone(),
            subject_type: self.subject_type.clone(),
            subject_owner: self.subject_owner,
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
            .field("subject_tenant", &self.subject_tenant)
            .field("source", &self.source)
            .field("subject", &self.subject)
            .field("subject_type", &self.subject_type)
            .field("subject_owner", &self.subject_owner)
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
            subject_tenant: event.subject_tenant,
            source: event.source.clone(),
            subject: event.subject.clone(),
            subject_type: event.subject_type.clone(),
            subject_owner: event.subject_owner,
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
    /// The most bytes an event may encode to, as [`encoded_len`](Self::encoded_len) counts
    /// them. A transactional outbox row holds at most 64 KiB (`toolkit_db::outbox`); the
    /// rest of it is [`OUTBOX_RESERVE_BYTES`](Self::OUTBOX_RESERVE_BYTES).
    pub const MAX_ENCODED_BYTES: usize = 60 * 1024;

    /// What an outbox row adds around an event of at most
    /// [`MAX_ENCODED_BYTES`](Self::MAX_ENCODED_BYTES): the broker's ingest row (the topic
    /// twice, the partition key, the stored event's empty server-stamped members, the
    /// broker's ingest stamp) and the SDK producer's envelope (the topic, the mode, the
    /// client agent).
    pub const OUTBOX_RESERVE_BYTES: usize = 4 * 1024;

    /// The most bytes a resolved partition key may take, as
    /// [`partition_key_len`](Self::partition_key_len) counts them. The broker stores the
    /// key beside the event in its ingest outbox row, so the key takes its share of
    /// [`OUTBOX_RESERVE_BYTES`](Self::OUTBOX_RESERVE_BYTES).
    pub const MAX_PARTITION_KEY_BYTES: usize = 1024;

    /// The event's size as the per-event limit counts it: the bytes of its JSON form as
    /// serde writes it, every member included.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when the event does not serialize, which no
    /// `serde_json::Value` payload does: a broken internal invariant.
    pub fn encoded_len(&self) -> Result<usize, EventBrokerError> {
        Self::json_len(self)
    }

    /// Checks the event against [`MAX_ENCODED_BYTES`](Self::MAX_ENCODED_BYTES).
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::EventTooLarge`] when it encodes to more; the error of
    /// [`encoded_len`](Self::encoded_len).
    pub fn check_size(&self) -> Result<(), EventBrokerError> {
        Self::check_encoded_len(self.encoded_len()?)
    }

    /// Checks a producer's unstamped event against
    /// [`MAX_ENCODED_BYTES`](Self::MAX_ENCODED_BYTES) as it will be sent: a deduplicating
    /// `mode` later stamps `meta`, so its largest stamp is counted in place of the
    /// event's `null` one. The event carries no `meta` yet.
    pub(crate) fn check_size_before_stamp(
        &self,
        mode: crate::api::ProducerMode,
    ) -> Result<(), EventBrokerError> {
        let stamp = match mode {
            crate::api::ProducerMode::Stateless => 0,
            crate::api::ProducerMode::Monotonic | crate::api::ProducerMode::Chained => {
                Self::json_len(&EventMeta::largest_stamp())? - "null".len()
            }
        };
        Self::check_encoded_len(self.encoded_len()? + stamp)
    }

    fn check_encoded_len(bytes: usize) -> Result<(), EventBrokerError> {
        if bytes > Self::MAX_ENCODED_BYTES {
            return Err(EventBrokerError::EventTooLarge {
                bytes,
                max: Self::MAX_ENCODED_BYTES,
            });
        }
        Ok(())
    }

    /// The size of a resolved partition key as its cap counts it: the bytes of its JSON
    /// string form without the quotes, as an outbox row stores it.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when the key does not serialize, which no string
    /// does: a broken internal invariant.
    pub fn partition_key_len(key: &str) -> Result<usize, EventBrokerError> {
        Ok(Self::json_len(key)? - "\"\"".len())
    }

    /// Checks a resolved partition key against
    /// [`MAX_PARTITION_KEY_BYTES`](Self::MAX_PARTITION_KEY_BYTES).
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::PartitionKeyTooLong`] when it takes more; the error of
    /// [`partition_key_len`](Self::partition_key_len).
    pub fn check_partition_key(key: &str) -> Result<(), EventBrokerError> {
        let bytes = Self::partition_key_len(key)?;
        if bytes > Self::MAX_PARTITION_KEY_BYTES {
            return Err(EventBrokerError::PartitionKeyTooLong {
                bytes,
                max: Self::MAX_PARTITION_KEY_BYTES,
            });
        }
        Ok(())
    }

    fn json_len(value: &(impl Serialize + ?Sized)) -> Result<usize, EventBrokerError> {
        serde_json::to_vec(value)
            .map(|encoded| encoded.len())
            .map_err(|err| {
                EventBrokerError::Internal(format!(
                    "measure an event's encoding ({})",
                    crate::error::json_error_category(&err)
                ))
            })
    }

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
                event_type: self.r#type.to_string(),
                category: err.classify().into(),
            })?;
        Ok(EventV1 {
            id: self.id,
            r#type: self.r#type.clone(),
            subject_tenant: self.subject_tenant,
            source: self.source.clone(),
            subject: self.subject.clone(),
            subject_type: self.subject_type.clone(),
            subject_owner: self.subject_owner,
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

    /// The event's publish-time members by their schema names: the ten members
    /// [`crate::models::Event`] exposes to a partition-key pointer.
    fn addressable(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "type": self.r#type,
            "subject_tenant": self.subject_tenant,
            "source": self.source,
            "subject": self.subject,
            "subject_type": self.subject_type,
            "subject_owner": self.subject_owner,
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
    subject_tenant: Option<Uuid>,
    source: Option<String>,
    subject: Option<String>,
    subject_type: Option<GtsTypeId>,
    subject_owner: Option<Uuid>,
    occurred_at: Option<DateTime<Utc>>,
    trace_parent: Option<String>,
}

impl<P: EventPayload> EventV1Builder<P> {
    fn new(r#type: GtsTypeId) -> Self {
        Self {
            r#type,
            data: None,
            id: None,
            subject_tenant: None,
            source: None,
            subject: None,
            subject_type: None,
            subject_owner: None,
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

    /// Sets the tenant of the subject the event is about. Required. The broker refuses the
    /// nil UUID with 400 `InvalidTenant`.
    #[must_use]
    pub fn subject_tenant(mut self, id: Uuid) -> Self {
        self.subject_tenant = Some(id);
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

    /// Sets the subject type to `S`'s type id: any GTS type the producer declares in
    /// Rust, typically its own domain type (`gts.acme.crm.accounts.tenant.v1~`). The
    /// broker only checks it against the event type's `allowed_subject_types`.
    /// Replaces any earlier subject type.
    #[must_use]
    pub fn subject_type<S: GtsSchema>(mut self) -> Self {
        self.subject_type = Some(GtsTypeId::new(S::TYPE_ID));
        self
    }

    /// Sets the subject type by id: any GTS type id the producer chooses, typically its
    /// own domain type (`gts.acme.crm.accounts.tenant.v1~`). The broker only checks it
    /// against the event type's `allowed_subject_types`. Replaces any earlier subject
    /// type.
    #[must_use]
    pub fn subject_type_of(mut self, subject_type: GtsTypeId) -> Self {
        self.subject_type = Some(subject_type);
        self
    }

    /// Sets the principal of the subject's tenant that owns the event. Without it the
    /// event is tenant-wide. `build()` refuses the nil UUID.
    #[must_use]
    pub fn subject_owner(mut self, owner: Uuid) -> Self {
        self.subject_owner = Some(owner);
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
    /// - [`EventBrokerError::InvalidEventField`] for `subject_tenant` when it is unset;
    /// - [`EventBrokerError::InvalidTextField`] for `source` when it is set and breaks
    ///   [`crate::validate::source`];
    /// - [`EventBrokerError::InvalidEventField`] for `subject` when it is unset, or
    ///   [`EventBrokerError::InvalidTextField`] when it breaks [`crate::validate::subject`];
    /// - [`EventBrokerError::InvalidEventField`] for `subject_type` when it is unset or
    ///   empty;
    /// - [`EventBrokerError::InvalidEventField`] for `subject_owner` when it is the nil
    ///   UUID;
    /// - [`EventBrokerError::InvalidEventField`] for `trace_parent` when it is set and is
    ///   not a W3C traceparent.
    pub fn build(self) -> Result<EventV1<P>, EventBrokerError> {
        // The value rules are `EventV1`'s; `check_envelope` shares those a decoded
        // event can break.
        EventV1::<P>::check_type(&self.r#type)?;
        let Some(subject_tenant) = self.subject_tenant else {
            return Err(EventV1::<P>::required("subject_tenant"));
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
        if let Some(owner) = self.subject_owner {
            EventV1::<P>::check_subject_owner(owner)?;
        }
        if let Some(trace_parent) = &self.trace_parent {
            EventV1::<P>::check_trace_parent(trace_parent)?;
        }

        Ok(EventV1 {
            id: self.id.unwrap_or_else(Uuid::now_v7),
            r#type: self.r#type,
            subject_tenant,
            source: self.source,
            subject,
            subject_type,
            subject_owner: self.subject_owner,
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
            .field("subject_tenant", &self.subject_tenant)
            .field("source", &self.source)
            .field("subject", &self.subject)
            .field("subject_type", &self.subject_type)
            .field("subject_owner", &self.subject_owner)
            .field("occurred_at", &self.occurred_at)
            .field("trace_parent", &self.trace_parent)
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
