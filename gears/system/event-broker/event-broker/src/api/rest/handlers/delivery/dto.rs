//! REST DTOs for the delivery-side API (consumer groups, subscriptions,
//! streaming). These types own serde/`utoipa` schema concerns and convert
//! to/from `domain::model`/`domain::delivery` types via `From`/`TryFrom`
//! impls - the same house convention every other gear's single
//! `api/rest/dto.rs` follows, split into `ingest`/`delivery` here since this
//! crate has two distinct handler groups rather than one.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use gts::{GtsIdPattern, GtsInstanceId};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::domain::delivery::{ControlCode, Frame, JoinOutcome, Position};
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{
    Assignment, BarrierMode, ConsumerGroup, ConsumerGroupCreateInput, ConsumerGroupKind, Event,
    FilterSpec, Interest, OwnershipFilter, Sequence, Subscription,
};

// ---------------------------------------------------------------------------
// consumer_groups.rs: CRUD for consumer groups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(response)]
pub enum ConsumerGroupKindDto {
    Anonymous,
    Named,
}

impl From<ConsumerGroupKind> for ConsumerGroupKindDto {
    fn from(kind: ConsumerGroupKind) -> Self {
        match kind {
            ConsumerGroupKind::Anonymous => ConsumerGroupKindDto::Anonymous,
            ConsumerGroupKind::Named => ConsumerGroupKindDto::Named,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
// Open question: consumer-group TTL is not modelled here yet (see DESIGN.md).
pub struct ConsumerGroupDto {
    pub id: String,
    pub kind: ConsumerGroupKindDto,
    pub tenant: uuid::Uuid,
    pub owner_principal: uuid::Uuid,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<ConsumerGroup> for ConsumerGroupDto {
    fn from(g: ConsumerGroup) -> Self {
        Self {
            id: g.id.to_string(),
            kind: g.kind.into(),
            tenant: g.tenant_id,
            owner_principal: g.owner_principal_id,
            description: g.description,
            created_at: g.created_at,
        }
    }
}

/// Request body for `POST /v1/consumer-groups`
/// (`docs/schemas/gts.cf.core.events.consumer_group.v1~.schema.json#/definitions/CreateRequest`)
/// - the whole body is optional (`docs/openapi.yaml`'s `requestBody.required:
/// false`); `client_agent` is required only once a body is sent at all
/// (`create_consumer_group`'s manual body-bytes handling, matching
/// `ingest::producers::reset_producer`'s established optional-body pattern).
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreateConsumerGroupRequest {
    pub client_agent: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl From<CreateConsumerGroupRequest> for ConsumerGroupCreateInput {
    fn from(req: CreateConsumerGroupRequest) -> Self {
        Self {
            client_agent: Some(req.client_agent),
            description: req.description,
        }
    }
}

// ---------------------------------------------------------------------------
// streaming.rs: GET /v1/events:stream, GET /v1/events:sse
// ---------------------------------------------------------------------------

/// `deny_unknown_fields` so a legacy `timeout`/`collect` (or any other
/// unrecognised) query parameter is rejected as a `400` rather than silently
/// ignored - the endpoint recognises only `subscription`
/// (`scenarios/consumer/stream/1.10`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamQuery {
    pub subscription: Uuid,
}

/// Wire shape of one event delivered in an `Event` frame.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub(crate) struct EventPayloadDto {
    id: Uuid,
    r#type: String,
    subject_tenant: Uuid,
    source: String,
    subject: String,
    subject_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_owner: Option<Uuid>,
    occurred_at: DateTime<Utc>,
    trace_parent: Option<String>,
    data: JsonValue,
    partition: Option<i32>,
    #[schema(value_type = Option<i64>)]
    sequence: Option<Sequence>,
    sequence_time: Option<DateTime<Utc>>,
}

impl From<Event> for EventPayloadDto {
    fn from(e: Event) -> Self {
        Self {
            id: e.id,
            r#type: e.r#type.into_string(),
            subject_tenant: e.subject_tenant,
            source: e.source,
            subject: e.subject,
            subject_type: e.subject_type.into_string(),
            subject_owner: e.subject_owner,
            occurred_at: e.occurred_at,
            trace_parent: e.trace_parent,
            data: e.data,
            partition: e.partition,
            sequence: e.sequence,
            sequence_time: e.sequence_time,
        }
    }
}

/// Wire shape of a partition position: the last delivered and last examined sequences.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub(crate) struct PositionDto {
    topic: String,
    partition: i32,
    #[schema(value_type = i64)]
    offset: Sequence,
    #[schema(value_type = i64)]
    last_examined: Sequence,
}

/// Wire shape for one frame (`event-broker-consumption-frames`): a JSON
/// object with a top-level `kind` discriminant.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "kind")]
pub(crate) enum FrameDto {
    /// A delivered event.
    Event {
        /// The event being delivered.
        payload: Box<EventPayloadDto>,
    },
    /// Keep-alive frame sent when no events are flowing.
    Heartbeat {
        /// Server time at which the heartbeat was emitted.
        at: DateTime<Utc>,
    },
    /// The member's partition assignment changed.
    Topology {
        /// Group topology version this assignment belongs to.
        topology_version: i64,
        /// Partitions now assigned to this member, with their positions.
        assigned: Vec<PositionDto>,
    },
    /// Control frame: `progress` reports positions and the stream continues;
    /// `terminal` ends the stream.
    Control {
        /// Machine-readable control code.
        code: &'static str,
        /// Positions to resume from.
        positions: Vec<PositionDto>,
        /// Optional human-readable explanation.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl From<crate::domain::streaming::frames::Position> for PositionDto {
    fn from(position: crate::domain::streaming::frames::Position) -> Self {
        Self {
            topic: position.topic.into_string(),
            partition: position.partition,
            offset: position.offset,
            last_examined: position.last_examined,
        }
    }
}

impl From<Frame> for FrameDto {
    fn from(frame: Frame) -> Self {
        match frame {
            Frame::Event(event) => FrameDto::Event {
                payload: Box::new((*event).into()),
            },
            Frame::Heartbeat { at } => FrameDto::Heartbeat { at },
            Frame::Topology {
                topology_version,
                positions,
            } => FrameDto::Topology {
                topology_version,
                assigned: positions.into_iter().map(PositionDto::from).collect(),
            },
            Frame::Control {
                code,
                positions,
                reason,
            } => FrameDto::Control {
                code: match code {
                    ControlCode::Progress => "progress",
                    ControlCode::Terminal => "terminal",
                },
                positions: positions.into_iter().map(PositionDto::from).collect(),
                reason: reason.map(|reason| reason.as_wire().to_owned()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// subscriptions.rs: JOIN, list, read, leave, seek
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct FilterSpecDto {
    pub engine: String,
    pub expression: String,
}

impl TryFrom<FilterSpecDto> for FilterSpec {
    type Error = DomainError;

    fn try_from(f: FilterSpecDto) -> Result<Self, DomainError> {
        Ok(Self {
            engine: GtsInstanceId::try_new(&f.engine).map_err(|_| DomainError::Validation {
                code: ErrorCode::InvalidBody,
                message: "filter engine must be a valid GTS instance id".to_owned(),
            })?,
            expression: f.expression,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[toolkit_macros::api_dto(request, response)]
pub enum OwnershipFilterDto {
    #[default]
    OnlyMine,
    EntireTenant,
    TenantSubtreeIncluded,
}

impl From<OwnershipFilter> for OwnershipFilterDto {
    fn from(filter: OwnershipFilter) -> Self {
        match filter {
            OwnershipFilter::OnlyMine => Self::OnlyMine,
            OwnershipFilter::EntireTenant => Self::EntireTenant,
            OwnershipFilter::TenantSubtreeIncluded => Self::TenantSubtreeIncluded,
        }
    }
}

impl From<OwnershipFilterDto> for OwnershipFilter {
    fn from(filter: OwnershipFilterDto) -> Self {
        match filter {
            OwnershipFilterDto::OnlyMine => Self::OnlyMine,
            OwnershipFilterDto::EntireTenant => Self::EntireTenant,
            OwnershipFilterDto::TenantSubtreeIncluded => Self::TenantSubtreeIncluded,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[toolkit_macros::api_dto(request)]
pub enum BarrierModeDto {
    #[default]
    Respect,
    Ignore,
}

impl From<BarrierModeDto> for BarrierMode {
    fn from(mode: BarrierModeDto) -> Self {
        match mode {
            BarrierModeDto::Respect => Self::Respect,
            BarrierModeDto::Ignore => Self::Ignore,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct InterestDto {
    pub topic: String,
    /// The tenant the interest reads; the caller's own tenant when absent.
    #[serde(default)]
    pub subject_tenant: Option<Uuid>,
    #[serde(default)]
    pub ownership_filter: OwnershipFilterDto,
    /// A removed field, read only so a body that still sends it gets a `400`
    /// that names it: `deny_unknown_fields` would make it a `422`.
    #[serde(default, deserialize_with = "InterestDto::present")]
    #[schema(ignore)]
    pub max_depth: Option<IgnoredAny>,
    #[serde(default)]
    pub barrier_mode: BarrierModeDto,
    pub types: Vec<String>,
    #[serde(default)]
    pub subject_types: Vec<String>,
    #[serde(default)]
    pub filter: Option<FilterSpecDto>,
}

impl InterestDto {
    /// The domain interest for a caller of tenant `caller_tenant`: an absent
    /// `subject_tenant` is the caller's own.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::InvalidBody`] for a body
    /// that sends `max_depth`, or the code of the first invalid topic, type
    /// pattern or filter.
    pub fn into_interest(self, caller_tenant: Uuid) -> Result<Interest, DomainError> {
        let i = self;
        if i.max_depth.is_some() {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidBody,
                message: "'max_depth' is not supported; use 'ownership_filter'".to_owned(),
            });
        }
        let topic = crate::domain::id_parse::parse_topic_id(&i.topic)?;
        let types = i
            .types
            .iter()
            .map(|selector| {
                GtsIdPattern::try_new(selector).map_err(|_| DomainError::Validation {
                    code: ErrorCode::BadTypePattern,
                    message: "event type selector must be a valid GTS pattern".to_owned(),
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        // An explicit `[]` reads as no selector, as the field's absence does.
        let subject_types = i
            .subject_types
            .iter()
            .map(|selector| {
                GtsIdPattern::try_new(selector).map_err(|_| DomainError::Validation {
                    code: ErrorCode::BadTypePattern,
                    message: "subject type selector must be a valid GTS pattern".to_owned(),
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        Ok(Interest {
            topic,
            subject_tenant: i.subject_tenant.unwrap_or(caller_tenant),
            ownership: i.ownership_filter.into(),
            barrier_mode: i.barrier_mode.into(),
            types,
            subject_types,
            filter: i.filter.map(FilterSpec::try_from).transpose()?,
        })
    }

    /// Reads a present field, `null` included, as `Some`, so only an absent
    /// field is `None`.
    fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<IgnoredAny>, D::Error> {
        IgnoredAny::deserialize(deserializer).map(Some)
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct JoinSubscriptionRequest {
    pub consumer_group: String,
    pub client_agent: String,
    pub interests: Vec<InterestDto>,
    #[serde(default)]
    pub session_timeout: Option<String>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct FilterSpecResponseDto {
    pub engine: String,
    pub expression: String,
}

impl From<FilterSpec> for FilterSpecResponseDto {
    fn from(f: FilterSpec) -> Self {
        Self {
            engine: f.engine.into_string(),
            expression: f.expression,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct InterestResponseDto {
    pub topic: String,
    /// The tenant the interest reads, resolved at JOIN.
    pub subject_tenant: Uuid,
    pub ownership_filter: OwnershipFilterDto,
    pub barrier_mode: String,
    pub types: Vec<String>,
    /// Echoed only when the interest carries a subject-type selector.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subject_types: Vec<String>,
    pub filter: Option<FilterSpecResponseDto>,
}

impl From<Interest> for InterestResponseDto {
    fn from(i: Interest) -> Self {
        Self {
            topic: i.topic.into_string(),
            subject_tenant: i.subject_tenant,
            ownership_filter: i.ownership.into(),
            barrier_mode: i.barrier_mode.as_wire().to_owned(),
            types: i
                .types
                .iter()
                .map(|pattern| pattern.pattern().to_owned())
                .collect(),
            subject_types: i
                .subject_types
                .iter()
                .map(|pattern| pattern.pattern().to_owned())
                .collect(),
            filter: i.filter.map(FilterSpecResponseDto::from),
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct AssignedPartitionDto {
    pub topic: String,
    pub partition: i32,
}

impl From<Assignment> for AssignedPartitionDto {
    fn from(a: Assignment) -> Self {
        Self {
            topic: a.topic.into_string(),
            partition: a.partition,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SubscriptionDto {
    pub id: Uuid,
    pub consumer_group: String,
    pub client_agent: String,
    pub interests: Vec<InterestResponseDto>,
    pub assigned: Vec<AssignedPartitionDto>,
    pub topology_version: i64,
    pub created_at: DateTime<Utc>,
}

/// One interest of the JOIN response: the echo, then the entries of its
/// `types` that matched no event type registered on the topic.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct JoinInterestResponseDto {
    #[serde(flatten)]
    pub interest: InterestResponseDto,
    /// The entries of `types` that matched no registered event type; `[]` when all matched.
    pub unmatched_types: Vec<String>,
}

/// The JOIN `201` body: the subscription, with each interest's
/// `unmatched_types`. A read or a listing answers [`SubscriptionDto`].
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct JoinSubscriptionResponseDto {
    pub id: Uuid,
    pub consumer_group: String,
    pub client_agent: String,
    pub interests: Vec<JoinInterestResponseDto>,
    pub assigned: Vec<AssignedPartitionDto>,
    pub topology_version: i64,
    pub created_at: DateTime<Utc>,
}

impl From<JoinOutcome> for JoinSubscriptionResponseDto {
    fn from(outcome: JoinOutcome) -> Self {
        let mut unmatched: BTreeMap<usize, Vec<GtsIdPattern>> =
            outcome.unmatched_types.into_iter().collect();
        let s = outcome.subscription;
        Self {
            id: s.id,
            consumer_group: s.consumer_group.to_string(),
            client_agent: s.client_agent,
            interests: s
                .interests
                .into_iter()
                .enumerate()
                .map(|(position, interest)| JoinInterestResponseDto {
                    interest: InterestResponseDto::from(interest),
                    unmatched_types: unmatched
                        .remove(&position)
                        .unwrap_or_default()
                        .iter()
                        .map(|pattern| pattern.pattern().to_owned())
                        .collect(),
                })
                .collect(),
            assigned: s
                .assigned
                .into_iter()
                .map(AssignedPartitionDto::from)
                .collect(),
            topology_version: s.topology_version,
            created_at: s.created_at,
        }
    }
}

impl From<Subscription> for SubscriptionDto {
    fn from(s: Subscription) -> Self {
        Self {
            id: s.id,
            consumer_group: s.consumer_group.to_string(),
            client_agent: s.client_agent,
            interests: s
                .interests
                .into_iter()
                .map(InterestResponseDto::from)
                .collect(),
            assigned: s
                .assigned
                .into_iter()
                .map(AssignedPartitionDto::from)
                .collect(),
            topology_version: s.topology_version,
            created_at: s.created_at,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(untagged)]
pub enum SeekValueDto {
    #[schema(value_type = i64)]
    Exact(Sequence),
    Sentinel(String),
}

impl SeekValueDto {
    pub(super) fn into_domain(self) -> Result<Position, DomainError> {
        match self {
            SeekValueDto::Exact(v) => Ok(Position::Exact(v)),
            SeekValueDto::Sentinel(s) if s == "earliest" => Ok(Position::Earliest),
            SeekValueDto::Sentinel(s) if s == "latest" => Ok(Position::Latest),
            SeekValueDto::Sentinel(s) => {
                let ts = s
                    .strip_prefix("at:")
                    .ok_or_else(|| DomainError::Validation {
                        code: ErrorCode::InvalidSeekValue,
                        message: format!(
                            "'{s}' is not a valid SEEK value - expected an integer, \"earliest\", \
                         \"latest\", or \"at:<ISO-8601>\""
                        ),
                    })?;
                let parsed =
                    DateTime::parse_from_rfc3339(ts).map_err(|_| DomainError::Validation {
                        code: ErrorCode::InvalidTimestamp,
                        message: format!("'{ts}' is not a valid ISO-8601 timestamp"),
                    })?;
                Ok(Position::At(parsed.with_timezone(&Utc)))
            }
        }
    }
}

/// One partition's seek target within a topic. The owning topic is the map key
/// in [`SeekSubscriptionRequest::positions`], so it is not repeated here.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct PositionEntryDto {
    pub partition: i32,
    pub value: SeekValueDto,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct SeekSubscriptionRequest {
    /// The `topology_version` the caller last observed for the subscription
    /// (from JOIN or a subscription read). The broker fences the seek against a
    /// concurrent rebalance: a value differing from the subscription's current
    /// `topology_version` rejects the whole seek with `topology_version_mismatch`
    /// (412) so the caller re-reads the subscription and re-seeks.
    pub topology_version: i64,
    /// Seek targets keyed by topic GTS id, each mapping to its per-partition
    /// entries. A topic key named more than once rejects the whole seek with
    /// `InvalidBody` (400).
    // Kept as pairs in request order, a repeated key included: a map would keep
    // only the last array for that key and silently drop the earlier entries.
    #[serde(deserialize_with = "SeekSubscriptionRequest::deserialize_positions")]
    #[schema(value_type = BTreeMap<String, Vec<PositionEntryDto>>)]
    pub positions: Vec<(String, Vec<PositionEntryDto>)>,
}

/// `SeekSubscriptionRequest::positions` as `(topic, entries)` pairs.
type PositionPairs = Vec<(String, Vec<PositionEntryDto>)>;

impl SeekSubscriptionRequest {
    /// Reads `positions` as its `(topic, entries)` pairs in request order, a
    /// repeated key included.
    fn deserialize_positions<'de, D>(deserializer: D) -> Result<PositionPairs, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PairsVisitor;

        impl<'de> Visitor<'de> for PairsVisitor {
            type Value = PositionPairs;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object mapping topic ids to arrays of position entries")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry()? {
                    pairs.push(pair);
                }
                Ok(pairs)
            }
        }

        deserializer.deserialize_map(PairsVisitor)
    }
}

/// One partition's resolved integer offset within a topic; the topic is the map
/// key in [`SeekResponse::positions`].
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ResolvedPositionEntryDto {
    pub partition: i32,
    #[schema(value_type = i64)]
    pub value: Sequence,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SeekResponse {
    /// Resolved integer offsets keyed by topic GTS id, mirroring the request's
    /// `positions` shape.
    pub positions: BTreeMap<String, Vec<ResolvedPositionEntryDto>>,
}
