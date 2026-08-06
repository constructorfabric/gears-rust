//! REST DTOs for the delivery-side API (consumer groups, subscriptions,
//! streaming). These types own serde/`utoipa` schema concerns and convert
//! to/from `domain::model`/`domain::delivery` types via `From`/`TryFrom`
//! impls - the same house convention every other gear's single
//! `api/rest/dto.rs` follows, split into `ingest`/`delivery` here since this
//! crate has two distinct handler groups rather than one.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::delivery::{ControlCode, Frame, SeekValue};
use crate::domain::error::DomainError;
use crate::domain::model::{
    Assignment, ConsumerGroup, ConsumerGroupCreateInput, ConsumerGroupKind, Event, FilterSpec,
    Interest, Subscription,
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
// [todo]: what about TTL for the consumer group? check the DESIGN.md
pub struct ConsumerGroupDto {
    pub id: String,
    pub kind: ConsumerGroupKindDto,
    pub tenant_id: uuid::Uuid,
    pub owner_principal_id: uuid::Uuid,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<ConsumerGroup> for ConsumerGroupDto {
    fn from(g: ConsumerGroup) -> Self {
        Self {
            id: g.id.to_string(),
            kind: g.kind.into(),
            tenant_id: g.tenant_id,
            owner_principal_id: g.owner_principal_id,
            description: g.description,
            created_at: g.created_at,
        }
    }
}

/// Request body for `POST /v1/consumer-groups`
/// (`docs/schemas/consumer_group.v1.schema.json#/definitions/CreateRequest`)
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

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub subscription_id: Uuid,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub(crate) struct EventPayloadDto {
    id: Uuid,
    r#type: String,
    topic: String,
    partition_key: Option<String>,
    tenant_id: Uuid,
    source: String,
    subject: String,
    subject_type: String,
    occurred_at: DateTime<Utc>,
    trace_parent: Option<String>,
    data: JsonValue,
    partition: Option<i32>,
    sequence: Option<i64>,
    sequence_time: Option<DateTime<Utc>>,
}

impl From<Event> for EventPayloadDto {
    fn from(e: Event) -> Self {
        Self {
            id: e.id,
            r#type: e.r#type.into_string(),
            topic: e.topic.into_string(),
            partition_key: e.partition_key,
            tenant_id: e.tenant_id,
            source: e.source,
            subject: e.subject,
            subject_type: e.subject_type,
            occurred_at: e.occurred_at,
            trace_parent: e.trace_parent,
            data: e.data,
            partition: e.partition,
            sequence: e.sequence,
            sequence_time: e.sequence_time,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub(crate) struct PositionDto {
    topic: String,
    partition: i32,
    offset: i64,
    last_examined: i64,
}

/// Wire shape for one frame (`event-broker-consumption-frames`): a JSON
/// object with a top-level `kind` discriminant.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "kind")]
pub(crate) enum FrameDto {
    Event {
        payload: Box<EventPayloadDto>,
    },
    Heartbeat {
        at: DateTime<Utc>,
    },
    Topology {
        topology_version: i64,
        assigned: Vec<PositionDto>,
    },
    Control {
        code: &'static str,
        positions: Vec<PositionDto>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
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
                assigned,
            } => FrameDto::Topology {
                topology_version,
                assigned: assigned
                    .into_iter()
                    .map(|a| PositionDto {
                        topic: a.topic.into_string(),
                        partition: a.partition,
                        offset: a.offset,
                        last_examined: a.last_examined,
                    })
                    .collect(),
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
                positions: positions
                    .into_iter()
                    .map(|a| PositionDto {
                        topic: a.topic.into_string(),
                        partition: a.partition,
                        offset: a.offset,
                        last_examined: a.last_examined,
                    })
                    .collect(),
                reason,
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

impl From<FilterSpecDto> for FilterSpec {
    fn from(f: FilterSpecDto) -> Self {
        Self {
            engine: f.engine,
            expression: f.expression,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct InterestDto {
    pub topic: String,
    pub tenant_id: Uuid,
    /// Tenant-hierarchy traversal - accepted for wire-shape compatibility,
    /// not enforced (no authz-resolver integration exists in this domain
    /// layer yet).
    #[serde(default)]
    pub max_depth: Option<i32>,
    #[serde(default)]
    pub barrier_mode: Option<String>,
    pub types: Vec<String>,
    #[serde(default)]
    pub filter: Option<FilterSpecDto>,
}

impl TryFrom<InterestDto> for Interest {
    type Error = DomainError;

    fn try_from(i: InterestDto) -> Result<Self, DomainError> {
        let topic = GtsInstanceId::try_new(&i.topic).map_err(|err| DomainError::Validation {
            code: "InvalidBody",
            message: format!("'{}' is not a valid GTS instance id: {err}", i.topic),
        })?;
        Ok(Self {
            topic,
            tenant_id: i.tenant_id,
            types: i.types,
            filter: i.filter.map(FilterSpec::from),
        })
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
            engine: f.engine,
            expression: f.expression,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct InterestResponseDto {
    pub topic: String,
    pub tenant_id: Uuid,
    pub types: Vec<String>,
    pub filter: Option<FilterSpecResponseDto>,
}

impl From<Interest> for InterestResponseDto {
    fn from(i: Interest) -> Self {
        Self {
            topic: i.topic.into_string(),
            tenant_id: i.tenant_id,
            types: i.types,
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
    pub expires_at: DateTime<Utc>,
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
            // No live rebalance this pass (design.md "Streaming/rebalance
            // scope") - every subscription is generation 0.
            topology_version: 0,
            expires_at: s.expires_at,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(untagged)]
pub enum SeekValueDto {
    Exact(i64),
    Sentinel(String),
}

impl SeekValueDto {
    pub(super) fn into_domain(self) -> Result<SeekValue, DomainError> {
        match self {
            SeekValueDto::Exact(v) => Ok(SeekValue::Exact(v)),
            SeekValueDto::Sentinel(s) if s == "earliest" => Ok(SeekValue::Earliest),
            SeekValueDto::Sentinel(s) if s == "latest" => Ok(SeekValue::Latest),
            SeekValueDto::Sentinel(s) => {
                let ts = s
                    .strip_prefix("at:")
                    .ok_or_else(|| DomainError::Validation {
                        code: "InvalidSeekValue",
                        message: format!(
                            "'{s}' is not a valid SEEK value - expected an integer, \"earliest\", \
                         \"latest\", or \"at:<ISO-8601>\""
                        ),
                    })?;
                let parsed =
                    DateTime::parse_from_rfc3339(ts).map_err(|_| DomainError::Validation {
                        code: "InvalidTimestamp",
                        message: format!("'{ts}' is not a valid ISO-8601 timestamp"),
                    })?;
                Ok(SeekValue::AtTimestamp(parsed.with_timezone(&Utc)))
            }
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct PartitionPositionDto {
    pub topic: String,
    pub partition: i32,
    pub value: SeekValueDto,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct SeekSubscriptionRequest {
    pub partition_positions: Vec<PartitionPositionDto>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ResolvedPositionDto {
    pub topic: String,
    pub partition: i32,
    pub value: i64,
}
