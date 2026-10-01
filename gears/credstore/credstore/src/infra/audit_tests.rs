//! Unit tests for [`EventBrokerAuditSink`]: what is published, and that every
//! way the broker can fail is reduced to a counted, non-propagating failure.

#![allow(
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    reason = "test fake shared with the service audit tests"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::EventBrokerError;
use event_broker_sdk::api::{
    EventBrokerApi, FrameStream, IngestOutcome, JoinRequest, ProducerCursors, ProducerMode,
    SeekPosition, SeekResult, SubscriptionAssignment,
};
use event_broker_sdk::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, Event, EventType, Page,
    PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    AUDIT_TOPIC_ID, BrokerResolver, EventBrokerAuditSink, SECRET_AUDIT_EVENT_TYPE_ID,
    audit_event_type, audit_topic,
};
use crate::domain::ports::audit::{AuditEvent, AuditOperation, AuditOutcome, AuditSink};
use crate::domain::secret::test_support::FakeMetrics;

#[derive(Clone, Copy)]
pub enum Behavior {
    Accept,
    Reject,
    Hang,
}

pub struct FakeBroker {
    behavior: Behavior,
    published: Mutex<Vec<(Uuid, Event)>>,
}

impl FakeBroker {
    pub fn new(behavior: Behavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            published: Mutex::new(Vec::new()),
        })
    }

    pub fn published(&self) -> Vec<(Uuid, Event)> {
        self.published.lock().expect("lock").clone()
    }
}

const NOT_USED: &str = "FakeBroker only serves publish";

#[async_trait]
impl EventBrokerApi for FakeBroker {
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.published
            .lock()
            .expect("lock")
            .push((ctx.subject_tenant_id(), event.clone()));
        match self.behavior {
            Behavior::Accept => Ok(IngestOutcome::Accepted),
            Behavior::Reject => Err(EventBrokerError::Internal("rejected".to_owned())),
            Behavior::Hang => std::future::pending().await,
        }
    }

    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        _events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn reset_producer_chain(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
        _scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn create_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn get_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn list_consumer_groups(
        &self,
        _ctx: &SecurityContext,
        _query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn delete_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn get_subscription(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn list_subscriptions(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn seek(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
        _topology_version: i64,
        _positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn list_topic_segments(
        &self,
        _ctx: &SecurityContext,
        _topic: &str,
        _partition: u32,
        _range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        _id: &str,
    ) -> Result<EventType, EventBrokerError> {
        unimplemented!("{NOT_USED}")
    }
}

fn audit_event() -> AuditEvent {
    AuditEvent {
        subject_id: Uuid::from_u128(11),
        tenant_id: Uuid::from_u128(22),
        reference: "db-password".to_owned(),
        secret_type: "gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~".to_owned(),
        operation: AuditOperation::Read,
        outcome: AuditOutcome::Success,
    }
}

fn sink_for(
    broker: Option<Arc<FakeBroker>>,
    metrics: Arc<FakeMetrics>,
    timeout: Duration,
) -> EventBrokerAuditSink {
    let resolve: BrokerResolver =
        Arc::new(move || broker.clone().map(|b| b as Arc<dyn EventBrokerApi>));
    EventBrokerAuditSink::new(resolve, metrics, timeout)
}

#[tokio::test]
async fn publishes_one_event_with_the_audit_fields_and_no_secret() {
    let broker = FakeBroker::new(Behavior::Accept);
    let metrics = FakeMetrics::new();
    let sink = sink_for(
        Some(broker.clone()),
        metrics.clone(),
        Duration::from_secs(1),
    );

    sink.record(audit_event()).await;

    let published = broker.published();
    assert_eq!(published.len(), 1);
    let (acting_tenant, event) = &published[0];
    assert_eq!(*acting_tenant, Uuid::from_u128(22));
    assert_eq!(event.tenant_id, Uuid::from_u128(22));
    assert_eq!(event.type_id.as_ref(), SECRET_AUDIT_EVENT_TYPE_ID);
    assert_eq!(event.subject, "db-password");
    let data = event.data.as_ref().expect("payload");
    assert_eq!(
        data,
        &serde_json::json!({
            "subject_id": Uuid::from_u128(11),
            "reference": "db-password",
            "secret_type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~",
            "operation": "read",
            "outcome": "success",
        })
    );
    assert_eq!(metrics.audit_publish_failed_total(), 0);
}

#[tokio::test]
async fn a_rejecting_broker_is_counted_and_does_not_propagate() {
    let broker = FakeBroker::new(Behavior::Reject);
    let metrics = FakeMetrics::new();
    let sink = sink_for(
        Some(broker.clone()),
        metrics.clone(),
        Duration::from_secs(1),
    );

    sink.record(audit_event()).await;

    assert_eq!(broker.published().len(), 1);
    assert_eq!(metrics.audit_publish_failed_total(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_hanging_broker_is_cut_off_at_the_timeout_and_counted() {
    let broker = FakeBroker::new(Behavior::Hang);
    let metrics = FakeMetrics::new();
    let sink = sink_for(Some(broker), metrics.clone(), Duration::from_millis(500));

    let started = tokio::time::Instant::now();
    sink.record(audit_event()).await;

    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert_eq!(metrics.audit_publish_failed_total(), 1);
}

#[tokio::test]
async fn an_absent_broker_counts_every_event_and_logs_once() {
    let metrics = FakeMetrics::new();
    let sink = sink_for(None, metrics.clone(), Duration::from_secs(1));

    sink.record(audit_event()).await;
    assert!(
        sink.absent_logged
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    sink.record(audit_event()).await;
    sink.record(audit_event()).await;

    assert_eq!(metrics.audit_publish_failed_total(), 3);
}

#[tokio::test]
async fn a_broker_that_appears_later_is_used_from_then_on() {
    let metrics = FakeMetrics::new();
    let slot: Arc<Mutex<Option<Arc<FakeBroker>>>> = Arc::new(Mutex::new(None));
    let resolve: BrokerResolver = {
        let slot = Arc::clone(&slot);
        Arc::new(move || {
            slot.lock()
                .expect("lock")
                .clone()
                .map(|b| b as Arc<dyn EventBrokerApi>)
        })
    };
    let sink = EventBrokerAuditSink::new(resolve, metrics.clone(), Duration::from_secs(1));

    sink.record(audit_event()).await;
    assert_eq!(metrics.audit_publish_failed_total(), 1);

    let broker = FakeBroker::new(Behavior::Accept);
    *slot.lock().expect("lock") = Some(broker.clone());
    sink.record(audit_event()).await;

    assert_eq!(broker.published().len(), 1);
    assert_eq!(metrics.audit_publish_failed_total(), 1);
}

#[test]
fn declared_event_type_targets_the_audit_topic_and_has_no_secret_member() {
    let schema = audit_event_type();
    assert_eq!(
        schema["x-gts-traits"]["topic"].as_str(),
        Some(AUDIT_TOPIC_ID)
    );
    let text = schema.to_string();
    assert!(text.contains(SECRET_AUDIT_EVENT_TYPE_ID));
    let props = &schema["allOf"][1]["properties"]["data"]["properties"];
    assert!(props.get("secret").is_none() && props.get("value").is_none());
    assert_eq!(audit_topic()["id"].as_str(), Some(AUDIT_TOPIC_ID));
}
