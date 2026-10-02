#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::{
    ADMISSION_CONTROL_RESOURCE, FailureCondition, REFUSAL_EVENT_TYPE, RefusalEvent,
    RefusalEventCause,
};
use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    ConsumerGroup, ConsumerGroupId, ConsumerGroupQuery, CreateConsumerGroupRequest, Event,
    EventBrokerApi, EventBrokerError, FrameStream, IngestOutcome, JoinRequest, Page,
    PartitionRange, ProducerCursors, ProducerId, ProducerMode, ResetScope, SeekPosition,
    SeekResult, Subscription, SubscriptionAssignment, SubscriptionId, Topic, TopicSegment,
};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use parking_lot::Mutex;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::MockTypesRegistryClient;
use uuid::Uuid;

use super::{EVENT_SOURCE, QueuePublisher, run, to_broker_event};
use crate::domain::service::EventSink;
use crate::gear::gate_identity;
use crate::infra::metrics::AdmissionControlMetrics;

/// Fake broker: records published events (or, with `failing`, refuses them);
/// every other operation is refused.
#[derive(Default)]
struct FakeBroker {
    published: Mutex<Vec<(SecurityContext, Event)>>,
    failing: bool,
}

fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the fake broker".to_owned())
}

#[async_trait]
impl EventBrokerApi for FakeBroker {
    async fn register_producer(
        &self,
        _: &SecurityContext,
        _: ProducerMode,
        _: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        Err(unsupported())
    }
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        if self.failing {
            return Err(unsupported());
        }
        self.published.lock().push((ctx.clone(), event.clone()));
        Ok(IngestOutcome::Accepted)
    }
    async fn publish_batch(
        &self,
        _: &SecurityContext,
        _: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_producer_cursors(
        &self,
        _: &SecurityContext,
        _: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        Err(unsupported())
    }
    async fn reset_producer_chain(
        &self,
        _: &SecurityContext,
        _: ProducerId,
        _: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn create_consumer_group(
        &self,
        _: &SecurityContext,
        _: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_consumer_group(
        &self,
        _: &SecurityContext,
        _: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_consumer_groups(
        &self,
        _: &SecurityContext,
        _: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        Err(unsupported())
    }
    async fn delete_consumer_group(
        &self,
        _: &SecurityContext,
        _: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn join(
        &self,
        _: &SecurityContext,
        _: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_subscription(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_subscriptions(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        Err(unsupported())
    }
    async fn leave(&self, _: &SecurityContext, _: SubscriptionId) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn stream(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Err(unsupported())
    }
    async fn seek(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
        _: i64,
        _: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_topics(&self, _: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_topic_segments(
        &self,
        _: &SecurityContext,
        _: &str,
        _: u32,
        _: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_event_types(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_event_type(
        &self,
        _: &SecurityContext,
        _: &str,
    ) -> Result<EventType, EventBrokerError> {
        Err(unsupported())
    }
}

fn event() -> RefusalEvent {
    RefusalEvent {
        correlation_id: Uuid::from_u128(1),
        occurred_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        enforcing_gear: "gear".to_owned(),
        action: "create".to_owned(),
        resource_type: "gts.cf.core.test.widget.v1~".to_owned(),
        resource_id: None,
        resource_tenant_id: Uuid::from_u128(3),
        subject_id: Uuid::from_u128(4),
        subject_tenant_id: Uuid::from_u128(5),
        enforced: true,
        cause: RefusalEventCause::CouldNotRun,
        builtin_policy_id: None,
        condition: Some(FailureCondition::NoEngine),
        policy: None,
        property_names: vec!["name".to_owned()],
    }
}

#[test]
fn broker_event_shape() {
    let refusal = event();
    let event = to_broker_event(&refusal);
    assert_eq!(event.type_id, REFUSAL_EVENT_TYPE);
    assert_eq!(event.source, EVENT_SOURCE);
    assert_eq!(event.tenant_id, refusal.resource_tenant_id);
    assert_eq!(event.subject, refusal.correlation_id.to_string());
    assert_eq!(event.subject_type, ADMISSION_CONTROL_RESOURCE);
    assert_eq!(event.occurred_at.timestamp(), 1_700_000_000);
    assert_eq!(event.data.unwrap(), serde_json::to_value(&refusal).unwrap());
}

#[test]
fn a_full_queue_drops_instead_of_blocking() {
    let (publisher, mut rx) = QueuePublisher::new(2, Arc::new(AdmissionControlMetrics::global()));
    for _ in 0..5 {
        publisher.emit(event());
    }
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err(), "the overflow was dropped");
}

#[tokio::test]
async fn the_task_publishes_under_the_gate_identity_and_survives_a_missing_broker() {
    let (publisher, rx) = QueuePublisher::new(8, Arc::new(AdmissionControlMetrics::global()));
    let hub = Arc::new(ClientHub::new());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(MockTypesRegistryClient::new());
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        rx,
        Arc::clone(&hub),
        registry,
        gate_identity().unwrap(),
        true,
        Arc::new(AdmissionControlMetrics::global()),
        cancel.clone(),
    ));

    // No broker registered: the event is dropped, the task keeps running.
    publisher.emit(event());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let broker = Arc::new(FakeBroker::default());
    hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    publisher.emit(event());
    for _ in 0..100 {
        if !broker.published.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();
    task.await.unwrap();

    let seen = broker.published.lock();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.subject_id(), crate::gear::GATE_SUBJECT_ID);
}

/// Instruments recording into an in-memory exporter.
fn recording() -> (
    Arc<AdmissionControlMetrics>,
    InMemoryMetricExporter,
    SdkMeterProvider,
) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = Arc::new(AdmissionControlMetrics::new(
        &provider.meter("publisher-tests"),
    ));
    (metrics, exporter, provider)
}

/// Current value of the dropped-events counter.
fn dropped(exporter: &InMemoryMetricExporter, provider: &SdkMeterProvider) -> u64 {
    use opentelemetry_sdk::metrics::data::{
        AggregatedMetrics, MetricData, ResourceMetrics, ScopeMetrics, SumDataPoint,
    };

    provider.force_flush().expect("the reader flushes");
    let metrics = exporter.get_finished_metrics().expect("metrics exported");
    metrics
        .iter()
        .flat_map(ResourceMetrics::scope_metrics)
        .flat_map(ScopeMetrics::metrics)
        .filter(|metric| metric.name() == "admission_control_events_dropped_total")
        .filter_map(|metric| match metric.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                sum.data_points().map(SumDataPoint::value).max()
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

#[tokio::test]
async fn a_missing_or_failing_broker_counts_the_dropped_event() {
    let (metrics, exporter, provider) = recording();
    let (publisher, rx) = QueuePublisher::new(8, Arc::clone(&metrics));
    let hub = Arc::new(ClientHub::new());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(MockTypesRegistryClient::new());
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        rx,
        Arc::clone(&hub),
        registry,
        gate_identity().unwrap(),
        true,
        metrics,
        cancel.clone(),
    ));

    let wait_for = |count: u64| {
        let (exporter, provider) = (exporter.clone(), provider.clone());
        async move {
            for _ in 0..100 {
                if dropped(&exporter, &provider) >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };

    // No broker registered.
    publisher.emit(event());
    wait_for(1).await;
    assert_eq!(dropped(&exporter, &provider), 1);

    // A broker that refuses the event.
    hub.register::<dyn EventBrokerApi>(Arc::new(FakeBroker {
        failing: true,
        ..FakeBroker::default()
    }) as Arc<dyn EventBrokerApi>);
    publisher.emit(event());
    wait_for(2).await;
    assert_eq!(dropped(&exporter, &provider), 2);

    cancel.cancel();
    task.await.unwrap();
}
