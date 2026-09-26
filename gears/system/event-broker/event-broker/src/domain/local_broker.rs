//! In-process implementation of the [`EventBrokerApi`] SDK contract.
//!
//! `LocalBroker` is the gear's own implementation of the client-facing SDK
//! trait, holding the real [`IngestService`]/[`DeliveryService`] and doing the
//! same SDK-model <-> domain translation the REST handlers do, minus HTTP. It
//! is the in-process peer of the SDK's REST client: a caller resolving
//! `Arc<dyn EventBrokerApi>` cannot tell which one it holds, so the same
//! consumer/producer logic can run against a real broker with no socket in the
//! path (used by the SDK's own integration tests via the gear's test harness).
//!
//! The SDK-model <-> domain conversions are `From`/`TryFrom` impls rather than
//! free functions: the gear depends on the SDK, and every conversion names a
//! local domain type, so coherence permits the impl in either direction. The
//! one that cannot is `consumer_group_id` (`GtsInstanceId -> ConsumerGroupId`) -
//! both types are foreign here, so it stays a free function.
//!
//! Errors cross the two-hop `DomainError -> CanonicalError -> EventBrokerError`
//! path that the REST layer already defines, so a rejection here carries the
//! same domain-term body it would over the wire - no HTTP status codes, no
//! `Problem`, and no reproduction of raw request content.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::GtsInstanceId;
use toolkit_security::SecurityContext;

use event_broker_sdk as sdk;
use sdk::EventBrokerError;
use sdk::api::{
    AssignedPartition, ControlCode, EventBrokerApi, FrameStream, IngestOutcome, JoinRequest,
    PartitionCursor, PartitionPosition, ProducerCursors, ProducerMode, SeekPosition, SeekResult,
    SubscriptionAssignment, SubscriptionInterest, TopicCursors, WireEvent, WireFrame,
};
use sdk::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use sdk::models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest, Event,
    EventType, Page, PartitionAssignment, PartitionRange, ResetScope, Subscription, Topic,
    TopicSegment,
};
use sdk::sequence::Sequence;

use crate::domain::delivery::{
    DeliveryService, JoinRequest as DomainJoinRequest, SeekPosition as DomainSeekPosition,
    SeekTarget as DomainSeekTarget,
};
use crate::domain::error::DomainError;
use crate::domain::id_parse::{parse_consumer_group_id, parse_topic_id, parse_type_id};
use crate::domain::ingest::{
    IngestService, ProducerCursors as DomainProducerCursors, ProducerMode as DomainProducerMode,
    ProducerPartitionCursor, ProducerRegistrationInput, ProducerResetScope, ProducerTopicCursors,
    PublishAck, PublishRequest,
};
use crate::domain::model::{
    BarrierMode as DomainBarrierMode, ConsumerGroup as DomainConsumerGroup,
    ConsumerGroupCreateInput, ConsumerGroupKind as DomainConsumerGroupKind, Event as DomainEvent,
    FilterSpec, Interest, Meta, Subscription as DomainSubscription,
    TenantTraversalDepth as DomainDepth, Topic as DomainTopic, TopicSegmentManifest,
};
use crate::domain::streaming::frames::{
    ControlCode as DomainControlCode, Frame, Position as DomainPosition,
};

/// The documented JOIN default when the caller names no `session_timeout`
/// (`docs/schemas/gts.cf.core.events.subscription.v1~.schema.json`); the REST
/// layer resolves the same `PT30S` before building its domain request.
const DEFAULT_SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// In-process [`EventBrokerApi`] over the real ingest and delivery services.
pub struct LocalBroker {
    ingest: Arc<dyn IngestService>,
    delivery: Arc<dyn DeliveryService>,
}

impl LocalBroker {
    #[must_use]
    pub fn new(ingest: Arc<dyn IngestService>, delivery: Arc<dyn DeliveryService>) -> Self {
        Self { ingest, delivery }
    }
}

/// The single error hop this file uses: the domain error becomes the same
/// `CanonicalError` the REST layer would build, then the SDK's own inverse of
/// that canonical form. Keeps one mapping, shared with the wire.
fn to_sdk_err(err: DomainError) -> EventBrokerError {
    EventBrokerError::from(CanonicalError::from(err))
}

/// A group's GTS instance id -> the SDK's uuid-backed `ConsumerGroupId`. Only
/// anonymous groups carry a uuid; a named id has none, so it is reported as
/// unimplemented rather than silently mishandled (mirrors the REST client).
/// A free function, not a `TryFrom`: both types are foreign to this crate, so
/// coherence forbids the impl.
fn consumer_group_id(gts: &GtsInstanceId) -> Result<ConsumerGroupId, EventBrokerError> {
    ConsumerGroupId::try_from_gts(gts.as_ref()).ok_or_else(|| {
        EventBrokerError::Unimplemented("named consumer groups are not implemented".to_owned())
    })
}

// -- SDK -> domain (inbound requests) -----------------------------------------

/// Producer dedup mode; a stateless producer is never registered and has no id
/// to return, so it is rejected here - the same rejection the REST client
/// raises before touching the wire.
impl TryFrom<ProducerMode> for DomainProducerMode {
    type Error = EventBrokerError;

    fn try_from(mode: ProducerMode) -> Result<Self, Self::Error> {
        match mode {
            ProducerMode::Chained => Ok(DomainProducerMode::Chained),
            ProducerMode::Monotonic => Ok(DomainProducerMode::Monotonic),
            ProducerMode::Stateless => Err(EventBrokerError::InvalidProducerOptions {
                detail: "a stateless producer is not registered".to_owned(),
            }),
        }
    }
}

/// SDK `Event` -> domain `PublishRequest`, mirroring `ingest::dto`'s
/// `TryFrom<PublishEventRequest>`: envelope-field validation first, then the
/// GTS type parse, so a mis-encoded `source`/`subject` is reported for that
/// reason even when the type is also unregistered.
impl TryFrom<&Event> for PublishRequest {
    type Error = EventBrokerError;

    fn try_from(event: &Event) -> Result<Self, Self::Error> {
        sdk::validate::source(&event.source)
            .map_err(DomainError::from)
            .map_err(to_sdk_err)?;
        sdk::validate::subject(&event.subject)
            .map_err(DomainError::from)
            .map_err(to_sdk_err)?;
        let r#type = parse_type_id(&event.type_id).map_err(to_sdk_err)?;
        Ok(PublishRequest {
            id: event.id,
            r#type,
            tenant_id: event.tenant_id,
            source: event.source.clone(),
            subject: event.subject.clone(),
            subject_type: event.subject_type.clone(),
            occurred_at: event.occurred_at,
            trace_parent: event.trace_parent.clone(),
            data: event.data.clone().unwrap_or(serde_json::Value::Null),
            meta: event.meta.as_ref().map(|m| Meta {
                version: i32::from(m.version),
                producer_id: m.producer_id.unwrap_or_default(),
                previous: m.previous,
                sequence: m.sequence.unwrap_or(0),
            }),
        })
    }
}

/// SDK `SubscriptionInterest` -> domain `Interest` (a JOIN input).
impl TryFrom<&SubscriptionInterest> for Interest {
    type Error = EventBrokerError;

    fn try_from(interest: &SubscriptionInterest) -> Result<Self, Self::Error> {
        let topic = parse_topic_id(interest.topic()).map_err(to_sdk_err)?;
        let depth = match interest.tenant_depth() {
            sdk::api::TenantTraversalDepth::CurrentTenant => DomainDepth::CurrentTenant,
            sdk::api::TenantTraversalDepth::Descendants(n) => DomainDepth::Descendants(n),
            sdk::api::TenantTraversalDepth::UnlimitedDescendants => {
                DomainDepth::UnlimitedDescendants
            }
        };
        let barrier_mode = match interest.barrier_mode() {
            sdk::api::BarrierMode::Respect => DomainBarrierMode::Respect,
            sdk::api::BarrierMode::Ignore => DomainBarrierMode::Ignore,
        };
        Ok(Interest {
            topic,
            tenant_id: interest.tenant_id(),
            depth,
            barrier_mode,
            types: interest.types().to_vec(),
            filter: interest.filter().map(|f| FilterSpec {
                engine: f.engine().to_owned(),
                expression: f.expression().to_owned(),
            }),
        })
    }
}

/// SDK `JoinRequest` -> domain `JoinRequest`; the `PT30S` default is resolved
/// here when the caller names no timeout, matching the REST layer.
impl TryFrom<JoinRequest> for DomainJoinRequest {
    type Error = EventBrokerError;

    fn try_from(req: JoinRequest) -> Result<Self, Self::Error> {
        let consumer_group = parse_consumer_group_id(&req.group.to_gts()).map_err(to_sdk_err)?;
        let interests = req
            .interests
            .iter()
            .map(Interest::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DomainJoinRequest {
            consumer_group,
            client_agent: req.client_agent,
            interests,
            session_timeout: req.session_timeout.unwrap_or(DEFAULT_SESSION_TIMEOUT),
        })
    }
}

/// SDK `SeekPosition` -> domain `SeekTarget`.
impl TryFrom<&SeekPosition> for DomainSeekTarget {
    type Error = EventBrokerError;

    fn try_from(position: &SeekPosition) -> Result<Self, Self::Error> {
        let topic = parse_topic_id(&position.topic).map_err(to_sdk_err)?;
        Ok(DomainSeekTarget {
            topic,
            partition: i32::try_from(position.partition).unwrap_or_default(),
            value: position.value.clone(),
        })
    }
}

/// SDK create request -> domain create input. The domain carries the
/// `client_agent` as optional; the endpoint always supplies one.
impl From<CreateConsumerGroupRequest> for ConsumerGroupCreateInput {
    fn from(req: CreateConsumerGroupRequest) -> Self {
        ConsumerGroupCreateInput {
            client_agent: Some(req.client_agent),
            description: req.description,
        }
    }
}

/// SDK reset scope -> domain reset scope. The domain scope is either "all" or
/// one `(topic, partition)`, so a topic-only reset collapses to "all" - the
/// same collapse the server makes (`ingest::dto`'s `From<ResetProducerRequest>`:
/// only `(Some topic, Some partition)` is `TopicPartition`, every other shape is
/// `All`). Neither path can reset a single whole topic.
impl From<ResetScope<'_>> for ProducerResetScope {
    fn from(scope: ResetScope<'_>) -> Self {
        match scope {
            ResetScope::AllTopics | ResetScope::Topic(_) => ProducerResetScope::All,
            ResetScope::Partition { topic, partition } => ProducerResetScope::TopicPartition {
                topic: topic.to_owned(),
                partition: i32::try_from(partition).unwrap_or_default(),
            },
        }
    }
}

// -- domain -> SDK (outbound responses) ---------------------------------------

/// Admission outcome. Both `PublishAck` variants carry the stamped event; the
/// facade only needs the accepted/duplicate distinction.
impl From<PublishAck> for IngestOutcome {
    fn from(ack: PublishAck) -> Self {
        match ack {
            PublishAck::Accepted(_) => IngestOutcome::Accepted,
            PublishAck::Duplicate(_) => IngestOutcome::Duplicate,
        }
    }
}

impl From<ProducerPartitionCursor> for PartitionCursor {
    fn from(cursor: ProducerPartitionCursor) -> Self {
        PartitionCursor {
            partition: u32::try_from(cursor.partition).unwrap_or_default(),
            last_sequence: cursor.last_sequence,
        }
    }
}

impl From<ProducerTopicCursors> for TopicCursors {
    fn from(cursors: ProducerTopicCursors) -> Self {
        TopicCursors {
            topic: cursors.topic,
            partitions: cursors.partitions.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DomainProducerCursors> for ProducerCursors {
    fn from(cursors: DomainProducerCursors) -> Self {
        ProducerCursors {
            producer_id: ProducerId(cursors.producer_id),
            client_agent: cursors.client_agent,
            topics: cursors.topics.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<DomainConsumerGroup> for ConsumerGroup {
    type Error = EventBrokerError;

    fn try_from(group: DomainConsumerGroup) -> Result<Self, Self::Error> {
        Ok(ConsumerGroup {
            id: consumer_group_id(&group.id)?,
            tenant_id: group.tenant_id,
            owner_principal_id: group.owner_principal_id,
            kind: match group.kind {
                DomainConsumerGroupKind::Anonymous => ConsumerGroupKind::Anonymous,
                DomainConsumerGroupKind::Named => ConsumerGroupKind::Named,
            },
            description: group.description,
            created_at: group.created_at,
        })
    }
}

/// Domain `Interest` -> SDK `SubscriptionInterest` (the echo on a read).
impl TryFrom<Interest> for SubscriptionInterest {
    type Error = EventBrokerError;

    fn try_from(interest: Interest) -> Result<Self, Self::Error> {
        let depth = match interest.depth {
            DomainDepth::CurrentTenant => sdk::api::TenantTraversalDepth::CurrentTenant,
            DomainDepth::Descendants(n) => sdk::api::TenantTraversalDepth::Descendants(n),
            DomainDepth::UnlimitedDescendants => {
                sdk::api::TenantTraversalDepth::UnlimitedDescendants
            }
        };
        let barrier = match interest.barrier_mode {
            DomainBarrierMode::Respect => sdk::api::BarrierMode::Respect,
            DomainBarrierMode::Ignore => sdk::api::BarrierMode::Ignore,
        };
        let mut builder = SubscriptionInterest::builder()
            .topic(interest.topic)
            .tenant_id(interest.tenant_id)
            .tenant_depth(depth)
            .barrier_mode(barrier)
            .types(interest.types);
        if let Some(filter) = interest.filter {
            builder = builder.filter(sdk::api::Filter::new(filter.engine, filter.expression)?);
        }
        builder.build()
    }
}

impl TryFrom<DomainSubscription> for Subscription {
    type Error = EventBrokerError;

    fn try_from(sub: DomainSubscription) -> Result<Self, Self::Error> {
        Ok(Subscription {
            id: SubscriptionId(sub.id),
            consumer_group: consumer_group_id(&sub.consumer_group)?,
            client_agent: sub.client_agent,
            interests: sub
                .interests
                .into_iter()
                .map(SubscriptionInterest::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            assigned: sub
                .assigned
                .into_iter()
                .map(|a| PartitionAssignment {
                    topic: a.topic,
                    partition: u32::try_from(a.partition).unwrap_or_default(),
                })
                .collect(),
            topology_version: sub.topology_version,
            created_at: sub.created_at,
        })
    }
}

/// Domain `Subscription` -> the JOIN response. Infallible (unlike the full
/// `Subscription` conversion): the assignment shape carries no consumer-group
/// id to reject a named group on.
impl From<DomainSubscription> for SubscriptionAssignment {
    fn from(sub: DomainSubscription) -> Self {
        SubscriptionAssignment {
            subscription_id: SubscriptionId(sub.id),
            topology_version: sub.topology_version,
            assigned: sub
                .assigned
                .into_iter()
                .map(|a| AssignedPartition {
                    topic: a.topic,
                    partition: u32::try_from(a.partition).unwrap_or_default(),
                })
                .collect(),
        }
    }
}

impl From<DomainSeekPosition> for SeekResult {
    fn from(resolved: DomainSeekPosition) -> Self {
        SeekResult {
            topic: resolved.topic,
            partition: u32::try_from(resolved.partition).unwrap_or_default(),
            offset: resolved.offset,
        }
    }
}

impl From<DomainTopic> for Topic {
    fn from(topic: DomainTopic) -> Self {
        Topic {
            id: topic.id,
            description: topic.description,
            retention: topic.retention,
        }
    }
}

impl From<TopicSegmentManifest> for TopicSegment {
    fn from(manifest: TopicSegmentManifest) -> Self {
        TopicSegment {
            topic: manifest.topic.into_string(),
            partition: u32::try_from(manifest.partition).unwrap_or_default(),
            start_sequence: manifest.start_sequence,
            end_sequence: manifest.end_sequence,
            start_time: manifest.start_time,
            end_time: manifest.end_time,
            segments: manifest.segments,
        }
    }
}

/// Domain stream `Frame` -> SDK `WireFrame`. A struct/enum remap, not the byte
/// encoding the transports do - the consumer runtime reads `WireFrame`s
/// directly here.
impl From<Frame> for WireFrame {
    fn from(frame: Frame) -> Self {
        match frame {
            Frame::Event(event) => WireFrame::Event(WireEvent::from(*event)),
            Frame::Heartbeat { at } => WireFrame::Heartbeat {
                at: at.to_rfc3339(),
            },
            Frame::Topology {
                topology_version,
                positions,
            } => WireFrame::Topology {
                topology_version,
                assigned: positions.into_iter().map(Into::into).collect(),
            },
            Frame::Control {
                code,
                positions,
                reason,
            } => WireFrame::Control {
                code: match code {
                    DomainControlCode::Progress => ControlCode::Progress,
                    DomainControlCode::Terminal => ControlCode::Terminal,
                },
                positions: positions.into_iter().map(Into::into).collect(),
                reason: reason.map(|r| r.as_wire().to_owned()),
            },
        }
    }
}

/// A delivered domain `Event` -> the stream's `WireEvent`. Delivery always
/// stamps `partition`/`sequence`/`sequence_time`, so their absence would be a
/// bug; they fall back rather than panic (`sequence_time` to `occurred_at`).
impl From<DomainEvent> for WireEvent {
    fn from(event: DomainEvent) -> Self {
        WireEvent {
            id: event.id,
            type_id: event.r#type,
            tenant_id: event.tenant_id,
            subject: event.subject,
            subject_type: event.subject_type,
            partition: event
                .partition
                .map(|p| u32::try_from(p).unwrap_or_default())
                .unwrap_or_default(),
            sequence: event.sequence.unwrap_or(Sequence::NONE),
            occurred_at: event.occurred_at,
            sequence_time: event.sequence_time.unwrap_or(event.occurred_at),
            trace_parent: event.trace_parent,
            data: event.data,
        }
    }
}

impl From<DomainPosition> for PartitionPosition {
    fn from(position: DomainPosition) -> Self {
        PartitionPosition {
            topic: position.topic,
            partition: u32::try_from(position.partition).unwrap_or_default(),
            offset: position.offset,
            last_examined: position.last_examined,
        }
    }
}

#[async_trait]
impl EventBrokerApi for LocalBroker {
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        let registration = self
            .ingest
            .register_producer(
                ctx,
                ProducerRegistrationInput {
                    mode: DomainProducerMode::try_from(mode)?,
                    client_agent: client_agent.to_owned(),
                },
            )
            .await
            .map_err(to_sdk_err)?;
        Ok(ProducerId(registration.id))
    }

    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let ack = self
            .ingest
            .publish_event(ctx, PublishRequest::try_from(event)?)
            .await
            .map_err(to_sdk_err)?;
        Ok(ack.into())
    }

    // `publish_sync` is deliberately NOT overridden: ingest enqueues to the
    // async outbox and exposes no persist-confirmation hook (the REST layer
    // answers `501` for `Prefer: wait`), so the honest outcome is the default's
    // `Accepted`/`Duplicate` - reporting `Persisted` would assert a durable
    // write that has not necessarily completed. It inherits the trait default,
    // which delegates to `publish`.

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        let requests = events
            .iter()
            .map(PublishRequest::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        // All-or-nothing: any rejection is a `DomainError` for the whole batch;
        // the per-event `BatchResult` is not surfaced on success (matching the
        // REST contract), so an `Ok` is one `Accepted`.
        self.ingest
            .publish_batch(ctx, requests)
            .await
            .map_err(to_sdk_err)?;
        Ok(IngestOutcome::Accepted)
    }

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        self.ingest
            .get_producer_cursors(ctx, producer_id.0)
            .await
            .map(ProducerCursors::from)
            .map_err(to_sdk_err)
    }

    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        self.ingest
            .reset_producer(ctx, producer_id.0, scope.into())
            .await
            .map_err(to_sdk_err)
    }

    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        let group = self
            .delivery
            .create_consumer_group(ctx, req.into())
            .await
            .map_err(to_sdk_err)?;
        ConsumerGroup::try_from(group)
    }

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        let gts = parse_consumer_group_id(&id.to_gts()).map_err(to_sdk_err)?;
        let group = self
            .delivery
            .get_consumer_group(ctx, &gts)
            .await
            .map_err(to_sdk_err)?;
        ConsumerGroup::try_from(group)
    }

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        // The service returns the tenant-scoped set unpaginated; the REST layer
        // applies `$filter`/pagination. SIMPLIFICATION: this honours `limit`
        // by truncation and returns a single page - `filter`/`orderby`/`cursor`
        // are not applied here.
        let groups = self
            .delivery
            .list_consumer_groups(ctx)
            .await
            .map_err(to_sdk_err)?;
        let mut items = groups
            .into_iter()
            .map(ConsumerGroup::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let limit = query
            .limit
            .unwrap_or_else(|| u32::try_from(items.len()).unwrap_or(u32::MAX));
        items.truncate(limit as usize);
        Ok(Page {
            items,
            next_cursor: None,
            prev_cursor: None,
            limit,
        })
    }

    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        let gts = parse_consumer_group_id(&id.to_gts()).map_err(to_sdk_err)?;
        self.delivery
            .delete_consumer_group(ctx, &gts)
            .await
            .map_err(to_sdk_err)
    }

    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let sub = self
            .delivery
            .join(ctx, DomainJoinRequest::try_from(req)?)
            .await
            .map_err(to_sdk_err)?;
        Ok(sub.into())
    }

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        let sub = self
            .delivery
            .get_subscription(ctx, id.0)
            .await
            .map_err(to_sdk_err)?;
        Subscription::try_from(sub)
    }

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        let subs = self
            .delivery
            .list_subscriptions(ctx)
            .await
            .map_err(to_sdk_err)?;
        subs.into_iter().map(Subscription::try_from).collect()
    }

    async fn leave(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.delivery.leave(ctx, id.0).await.map_err(to_sdk_err)
    }

    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        let stream = self.delivery.stream(ctx, id.0).await.map_err(to_sdk_err)?;
        // The domain stream is infallible (`Item = Frame`); each frame becomes
        // one `Ok(WireFrame)` for the SDK's `Result`-carrying stream.
        Ok(Box::pin(tokio_stream::StreamExt::map(stream, |frame| {
            Ok(WireFrame::from(frame))
        })))
    }

    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        let targets = positions
            .iter()
            .map(DomainSeekTarget::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let resolved = self
            .delivery
            .seek(ctx, id.0, topology_version, targets)
            .await
            .map_err(to_sdk_err)?;
        Ok(resolved.into_iter().map(SeekResult::from).collect())
    }

    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Ok(self
            .ingest
            .list_topics(ctx)
            .await
            .map_err(to_sdk_err)?
            .into_iter()
            .map(Topic::from)
            .collect())
    }

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        _range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        // The service reports the whole `(topic, partition)` manifest; `range`
        // is a REST-side window this in-process path does not apply.
        let topic = parse_topic_id(topic).map_err(to_sdk_err)?;
        self.ingest
            .list_topic_segments(ctx, &topic, i32::try_from(partition).unwrap_or_default())
            .await
            .map(TopicSegment::from)
            .map_err(to_sdk_err)
    }

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        self.ingest.list_event_types(ctx).await.map_err(to_sdk_err)
    }

    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        // No single-type service method: select from the list, matching the
        // REST client's own fallback.
        self.ingest
            .list_event_types(ctx)
            .await
            .map_err(to_sdk_err)?
            .into_iter()
            .find(|t| t.id.as_ref() == id)
            .ok_or_else(|| EventBrokerError::EventTypeUnknown {
                type_id: id.to_owned(),
                detail: "no such event type".to_owned(),
            })
    }
}
