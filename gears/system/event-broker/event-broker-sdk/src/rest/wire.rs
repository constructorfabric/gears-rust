//! Private serde wire DTOs mirroring the event-broker REST bodies.
//!
//! These types are the transport's own; they carry serde so the public SDK
//! models do not. Field names, renames, `deny_unknown_fields`, the `kind`-tagged
//! frame enum and the untagged `max_depth`/`seek value` shapes reproduce the
//! server DTOs in `event-broker/src/api/rest/handlers/{ingest,delivery}/dto.rs`
//! exactly (the server applies `#[serde(rename_all = "snake_case")]` via
//! `#[toolkit_macros::api_dto]`, matched here). Sequences travel as bare `i64`
//! and timestamps as RFC 3339, so nothing here depends on a public SDK type's
//! representation.
//!
//! Only the transport speaks these types; none is re-exported from the crate.

use std::collections::BTreeMap;

use gts::{GtsInstanceId, GtsTypeId};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::api::{ControlCode, PartitionPosition, WireFrame};
use crate::error::EventBrokerError;
use crate::gts::{EventV1, StoredPosition};
use crate::sequence::Sequence;

// ---------------------------------------------------------------------------
// Pagination envelope (toolkit_odata::Page) - `{ items, page_info }`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PageWire<T> {
    pub items: Vec<T>,
    pub page_info: PageInfoWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PageInfoWire {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u64,
}

// ---------------------------------------------------------------------------
// Ingest: POST /v1/events, /v1/events:batch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct MetaWire {
    pub version: i32,
    pub producer_id: Option<Uuid>,
    pub previous: Option<i64>,
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct PublishEventWire {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub type_id: GtsTypeId,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: GtsTypeId,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaWire>,
}

/// The publish body of an [`EventV1`]. It refuses what [`EventV1::check_publishable`]
/// refuses, and a `meta.version` past the wire's `i32`.
impl TryFrom<&EventV1> for PublishEventWire {
    type Error = EventBrokerError;

    fn try_from(event: &EventV1) -> Result<Self, EventBrokerError> {
        event.check_publishable()?;
        // `check_publishable` already refused a missing source; the type still holds an
        // `Option`.
        let Some(source) = event.source() else {
            return Err(EventBrokerError::InvalidEventField {
                field: "source",
                detail: "required".to_owned(),
            });
        };
        let meta = event
            .meta()
            .map(|meta| {
                let version = i32::try_from(meta.version).map_err(|_| {
                    EventBrokerError::InvalidEventField {
                        field: "meta",
                        detail: "version out of range".to_owned(),
                    }
                })?;
                Ok::<_, EventBrokerError>(MetaWire {
                    version,
                    producer_id: meta.producer_id,
                    previous: meta.previous,
                    sequence: meta.sequence,
                })
            })
            .transpose()?;
        Ok(Self {
            id: event.id(),
            type_id: event.event_type().clone(),
            tenant_id: event.tenant_id(),
            source: source.to_owned(),
            subject: event.subject().to_owned(),
            subject_type: event.subject_type().clone(),
            occurred_at: event.occurred_at(),
            trace_parent: event.trace_parent().map(str::to_owned),
            data: event.data().cloned(),
            meta,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct PublishBatchWire {
    pub events: Vec<PublishEventWire>,
}

// ---------------------------------------------------------------------------
// Ingest: producers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProducerModeWire {
    Chained,
    Monotonic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct RegisterProducerWire {
    pub mode: ProducerModeWire,
    pub client_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RegisterProducerResponseWire {
    pub id: Uuid,
    pub mode: ProducerModeWire,
    pub client_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ProducerTopicCursorsWire {
    pub topic: GtsInstanceId,
    pub last_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ProducerCursorsResponseWire {
    pub producer_id: Uuid,
    pub client_agent: String,
    pub topics: Vec<ProducerTopicCursorsWire>,
}

impl From<ProducerCursorsResponseWire> for crate::api::ProducerCursors {
    fn from(w: ProducerCursorsResponseWire) -> Self {
        Self {
            producer_id: crate::ids::ProducerId(w.producer_id),
            client_agent: w.client_agent,
            topics: w
                .topics
                .into_iter()
                .map(|t| crate::api::TopicCursors {
                    topic: t.topic,
                    last_sequence: t.last_sequence,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct ResetProducerWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl ResetProducerWire {
    pub(crate) fn from_scope(scope: crate::models::ResetScope<'_>) -> Self {
        match scope {
            crate::models::ResetScope::AllTopics => Self::default(),
            crate::models::ResetScope::Topic(topic) => Self {
                topic: Some(topic.to_owned()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Ingest: event types, topics, segments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct EventTypeWire {
    pub id: GtsTypeId,
    pub topic: GtsInstanceId,
    pub description: Option<String>,
    // Subject-type selectors are GTS patterns (may carry a wildcard), and
    // `GtsIdPattern` has no serde, so they ride the wire as strings and convert
    // at the client boundary.
    pub allowed_subject_types: Vec<String>,
    pub partition_key: String,
    pub data_schema: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TopicWire {
    pub id: GtsInstanceId,
    pub description: Option<String>,
    pub retention: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TopicSegmentsResponseWire {
    pub topic: String,
    pub partition: i32,
    pub start_sequence: i64,
    pub end_sequence: i64,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub segments: Vec<JsonValue>,
}

// ---------------------------------------------------------------------------
// Delivery: consumer groups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct CreateConsumerGroupWire {
    pub client_agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl CreateConsumerGroupWire {
    pub(crate) fn from_request(req: crate::models::CreateConsumerGroupRequest) -> Self {
        Self {
            client_agent: req.client_agent,
            description: req.description,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsumerGroupKindWire {
    Anonymous,
    Named,
}

impl ConsumerGroupKindWire {
    /// The wire token, as `$filter` compares it (`kind eq 'anonymous'`).
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Named => "named",
        }
    }
}

impl From<crate::models::ConsumerGroupKind> for ConsumerGroupKindWire {
    fn from(k: crate::models::ConsumerGroupKind) -> Self {
        match k {
            crate::models::ConsumerGroupKind::Anonymous => Self::Anonymous,
            crate::models::ConsumerGroupKind::Named => Self::Named,
        }
    }
}

impl From<ConsumerGroupKindWire> for crate::models::ConsumerGroupKind {
    fn from(k: ConsumerGroupKindWire) -> Self {
        match k {
            ConsumerGroupKindWire::Anonymous => Self::Anonymous,
            ConsumerGroupKindWire::Named => Self::Named,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ConsumerGroupWire {
    pub id: String,
    pub kind: ConsumerGroupKindWire,
    pub tenant_id: Uuid,
    pub owner_principal_id: Uuid,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Delivery: subscriptions (JOIN / read / list)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct FilterSpecWire {
    pub engine: GtsInstanceId,
    pub expression: String,
}

/// Untagged tri-state: an integer depth, or an explicit JSON `null` for
/// unbounded. Matches the server's `MaxDepthDto` (whose `#[serde(default)]` is
/// `Levels(0)` - current tenant only).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MaxDepthWire {
    Unlimited,
    Levels(i32),
}

impl Default for MaxDepthWire {
    fn default() -> Self {
        Self::Levels(0)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BarrierModeWire {
    #[default]
    Respect,
    Ignore,
}

impl From<crate::api::BarrierMode> for BarrierModeWire {
    fn from(m: crate::api::BarrierMode) -> Self {
        match m {
            crate::api::BarrierMode::Respect => Self::Respect,
            crate::api::BarrierMode::Ignore => Self::Ignore,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct InterestWire {
    pub topic: GtsInstanceId,
    pub tenant_id: Uuid,
    #[serde(default)]
    pub max_depth: MaxDepthWire,
    #[serde(default)]
    pub barrier_mode: BarrierModeWire,
    pub types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<FilterSpecWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct JoinSubscriptionWire {
    pub consumer_group: String,
    pub client_agent: String,
    pub interests: Vec<InterestWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct FilterSpecResponseWire {
    pub engine: GtsInstanceId,
    pub expression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct InterestResponseWire {
    pub topic: GtsInstanceId,
    pub tenant_id: Uuid,
    pub max_depth: Option<i32>,
    pub barrier_mode: String,
    pub types: Vec<String>,
    pub filter: Option<FilterSpecResponseWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct AssignedPartitionWire {
    pub topic: GtsInstanceId,
    pub partition: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SubscriptionWire {
    pub id: Uuid,
    pub consumer_group: String,
    pub client_agent: String,
    pub interests: Vec<InterestResponseWire>,
    pub assigned: Vec<AssignedPartitionWire>,
    pub topology_version: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Delivery: seek
// ---------------------------------------------------------------------------

/// Untagged: an exact integer offset, or a sentinel string
/// (`earliest`/`latest`/`at:<rfc3339>`). Matches the server's `SeekValueDto`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SeekValueWire {
    Exact(i64),
    Sentinel(String),
}

impl From<&crate::api::Position> for SeekValueWire {
    fn from(p: &crate::api::Position) -> Self {
        match p {
            crate::api::Position::Exact(seq) => Self::Exact(seq.as_i64()),
            crate::api::Position::Earliest => Self::Sentinel("earliest".to_owned()),
            crate::api::Position::Latest => Self::Sentinel("latest".to_owned()),
            crate::api::Position::At(at) => Self::Sentinel(format!("at:{}", at.to_rfc3339())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct PositionEntryWire {
    pub partition: i32,
    pub value: SeekValueWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct SeekSubscriptionWire {
    pub topology_version: i64,
    /// Seek targets keyed by topic GTS id (a `BTreeMap` so the serialized order
    /// is deterministic), each mapping to its per-partition entries.
    pub positions: BTreeMap<String, Vec<PositionEntryWire>>,
}

impl SeekSubscriptionWire {
    pub(crate) fn new(topology_version: i64, positions: &[crate::api::SeekPosition]) -> Self {
        let mut grouped: BTreeMap<String, Vec<PositionEntryWire>> = BTreeMap::new();
        for p in positions {
            grouped
                .entry(p.topic.as_ref().to_owned())
                .or_default()
                .push(PositionEntryWire {
                    partition: i32::try_from(p.partition).unwrap_or(i32::MAX),
                    value: SeekValueWire::from(&p.value),
                });
        }
        Self {
            topology_version,
            positions: grouped,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct ResolvedPositionEntryWire {
    pub partition: i32,
    pub value: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SeekResponseWire {
    pub positions: BTreeMap<String, Vec<ResolvedPositionEntryWire>>,
}

impl SeekResponseWire {
    pub(crate) fn into_results(self) -> Result<Vec<crate::api::SeekResult>, EventBrokerError> {
        let mut out = Vec::new();
        for (topic, entries) in self.positions {
            let topic = GtsInstanceId::try_new(&topic).map_err(|err| {
                EventBrokerError::Transport(format!(
                    "seek response carried an invalid topic: {err}"
                ))
            })?;
            for e in entries {
                out.push(crate::api::SeekResult {
                    topic: topic.clone(),
                    partition: u32::try_from(e.partition).unwrap_or_default(),
                    offset: Sequence::assigned(e.value),
                });
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Delivery: stream frames (kind-tagged), mirroring the server `FrameDto`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct EventPayloadWire {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub type_id: GtsTypeId,
    pub topic: GtsInstanceId,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: GtsTypeId,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: JsonValue,
    pub partition: Option<i32>,
    pub sequence: Option<i64>,
    pub sequence_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl EventPayloadWire {
    /// The delivered event, built through [`EventV1`]'s builder and stamped with its
    /// position. A missing `sequence_time` takes `occurred_at`. A `trace_parent` that
    /// is not a W3C traceparent is dropped with a warning: an event stored before ingest
    /// checked the field can carry one, and it is optional telemetry, so it must not
    /// stop the consumer.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Transport`] when the payload carries no sequence or no
    /// partition, a partition outside `u32`, or an envelope member the builder refuses.
    /// The broker admitted the event under the rules the builder mirrors, so a refusal
    /// means the two disagree; the error names the field, never its value.
    fn into_event(self) -> Result<EventV1, EventBrokerError> {
        let sequence = self.sequence.ok_or_else(|| {
            EventBrokerError::Transport("stream event frame carried no sequence".to_owned())
        })?;
        let partition = self.partition.ok_or_else(|| {
            EventBrokerError::Transport("stream event frame carried no partition".to_owned())
        })?;
        let partition = u32::try_from(partition).map_err(|_| {
            EventBrokerError::Transport(
                "stream event frame carried a partition outside u32".to_owned(),
            )
        })?;
        let trace_parent = match self.trace_parent {
            Some(trace_parent) if <EventV1>::check_trace_parent(&trace_parent).is_err() => {
                tracing::warn!(
                    event_id = %self.id,
                    type_id = %self.type_id,
                    "a delivered event carries a malformed trace_parent; delivering it without one"
                );
                None
            }
            trace_parent => trace_parent,
        };
        let builder = if self.data.is_null() {
            EventV1::builder_without_data(self.type_id)
        } else {
            EventV1::builder_for(self.type_id, self.data)
        };
        let builder = builder
            .id(self.id)
            .tenant_id(self.tenant_id)
            .source(self.source)
            .subject(self.subject)
            .subject_type_id(self.subject_type)
            .occurred_at(self.occurred_at);
        let builder = match trace_parent {
            Some(trace_parent) => builder.trace_parent(trace_parent),
            None => builder,
        };
        let event = builder.build().map_err(|err| {
            EventBrokerError::Transport(format!("stream event frame broke an envelope rule: {err}"))
        })?;
        Ok(event.stamp_position(StoredPosition {
            partition,
            sequence: Sequence::assigned(sequence),
            sequence_time: self.sequence_time.unwrap_or(self.occurred_at),
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PositionWire {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
    pub last_examined: i64,
}

impl PositionWire {
    fn into_domain(self) -> Result<PartitionPosition, EventBrokerError> {
        Ok(PartitionPosition {
            topic: gts::GtsInstanceId::try_new(&self.topic).map_err(|err| {
                EventBrokerError::Transport(format!("stream frame carried an invalid topic: {err}"))
            })?,
            partition: u32::try_from(self.partition).unwrap_or_default(),
            offset: Sequence::assigned(self.offset),
            last_examined: Sequence::assigned(self.last_examined),
        })
    }
}

/// One frame as the delivery stream carries it, discriminated by a top-level
/// `kind` field. Both the multipart and SSE framings wrap this same body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FrameWire {
    Event {
        payload: Box<EventPayloadWire>,
    },
    Heartbeat {
        at: chrono::DateTime<chrono::Utc>,
    },
    Topology {
        topology_version: i64,
        assigned: Vec<PositionWire>,
    },
    Control {
        code: String,
        positions: Vec<PositionWire>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl FrameWire {
    /// Converts a decoded wire frame into the public [`WireFrame`]. Malformed
    /// content (an event missing its sequence or breaking an envelope rule, an
    /// unknown control code, an invalid topic) becomes a transport error rather
    /// than a silent default.
    pub(crate) fn into_frame(self) -> Result<WireFrame, EventBrokerError> {
        match self {
            FrameWire::Event { payload } => payload.into_event().map(WireFrame::Event),
            FrameWire::Heartbeat { at } => Ok(WireFrame::Heartbeat {
                at: at.to_rfc3339(),
            }),
            FrameWire::Topology {
                topology_version,
                assigned,
            } => Ok(WireFrame::Topology {
                topology_version,
                assigned: assigned
                    .into_iter()
                    .map(PositionWire::into_domain)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            FrameWire::Control {
                code,
                positions,
                reason,
            } => Ok(WireFrame::Control {
                code: match code.as_str() {
                    "progress" => ControlCode::Progress,
                    "terminal" => ControlCode::Terminal,
                    other => {
                        return Err(EventBrokerError::Transport(format!(
                            "stream control frame carried an unknown code: {other}"
                        )));
                    }
                },
                positions: positions
                    .into_iter()
                    .map(PositionWire::into_domain)
                    .collect::<Result<Vec<_>, _>>()?,
                reason,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
