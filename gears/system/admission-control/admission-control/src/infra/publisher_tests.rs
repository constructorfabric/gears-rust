#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use serde_json::Value;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

use super::{EVENT_SOURCE, QueuePublisher, QueuedEvent, run, to_broker_event};
use crate::domain::service::{EventEnvelope, EventSink};
use crate::gear::gate_identity;
use crate::infra::metrics::AdmissionControlMetrics;

/// Fake broker: records published events (or, with `failing`, refuses them;
/// with `stalled`, never answers); every other operation is refused.
#[derive(Default)]
struct FakeBroker {
    published: Mutex<Vec<(SecurityContext, Event)>>,
    failing: bool,
    stalled: bool,
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
        if self.stalled {
            std::future::pending::<()>().await;
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

fn envelope() -> EventEnvelope {
    EventEnvelope {
        correlation_id: Uuid::from_u128(1),
        occurred_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        tenant_id: Uuid::from_u128(3),
    }
}

fn event() -> RefusalEvent {
    RefusalEvent {
        enforcing_gear: "gear".to_owned(),
        action: "create".to_owned(),
        resource_type: "gts.cf.core.test.widget.v1~".to_owned(),
        resource_id: None,
        subject_id: Uuid::from_u128(4),
        subject_tenant_id: Uuid::from_u128(5),
        enforced: true,
        cause: RefusalEventCause::CouldNotRun,
        condition: Some(FailureCondition::NoEngine),
        policy: None,
        property_names: vec!["name".to_owned()],
    }
}

#[test]
fn broker_event_shape() {
    let queued = QueuedEvent {
        envelope: envelope(),
        event: event(),
    };
    let event = to_broker_event(&queued);
    assert_eq!(event.type_id, REFUSAL_EVENT_TYPE);
    assert_eq!(event.source, EVENT_SOURCE);
    assert_eq!(event.tenant_id, queued.envelope.tenant_id);
    assert_eq!(event.subject, queued.envelope.correlation_id.to_string());
    assert_eq!(event.subject_type, ADMISSION_CONTROL_RESOURCE);
    assert_eq!(event.occurred_at.timestamp(), 1_700_000_000);
    // The envelope fields are not repeated in the payload.
    assert_eq!(
        event.data.unwrap(),
        serde_json::to_value(&queued.event).unwrap()
    );
}

#[test]
fn a_full_queue_drops_instead_of_blocking() {
    let (publisher, mut rx) = QueuePublisher::new(2, Arc::new(AdmissionControlMetrics::global()));
    for _ in 0..5 {
        publisher.emit(envelope(), event());
    }
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err(), "the overflow was dropped");
}

#[tokio::test]
async fn the_task_publishes_under_the_gate_identity_and_survives_a_missing_broker() {
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

    // No broker registered: the event is dropped, the task keeps running.
    // Wait for the drop to be counted, so the broker below is registered
    // only after it.
    publisher.emit(envelope(), event());
    for _ in 0..100 {
        if dropped(&exporter, &provider) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(dropped(&exporter, &provider), 1);

    let broker = Arc::new(FakeBroker::default());
    hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    publisher.emit(envelope(), event());
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
    assert_eq!(seen[0].0.token_scopes(), [crate::gear::GATE_TOKEN_SCOPE]);
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
    publisher.emit(envelope(), event());
    wait_for(1).await;
    assert_eq!(dropped(&exporter, &provider), 1);

    // A broker that refuses the event.
    hub.register::<dyn EventBrokerApi>(Arc::new(FakeBroker {
        failing: true,
        ..FakeBroker::default()
    }) as Arc<dyn EventBrokerApi>);
    publisher.emit(envelope(), event());
    wait_for(2).await;
    assert_eq!(dropped(&exporter, &provider), 2);

    cancel.cancel();
    task.await.unwrap();
}

/// Registry whose `register` answers from a script, one outcome per call
/// (the last repeats); every other operation is the mock's.
struct ScriptedRegistry {
    inner: MockTypesRegistryClient,
    script: Vec<Registration>,
    calls: AtomicUsize,
}

#[derive(Clone, Copy)]
enum Registration {
    Unreachable,
    /// Never answers.
    Stalled,
    Accept,
    Reject,
}

impl ScriptedRegistry {
    fn new(script: Vec<Registration>) -> Arc<Self> {
        Arc::new(Self {
            inner: MockTypesRegistryClient::new(),
            script,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TypesRegistryClient for ScriptedRegistry {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self.script[call.min(self.script.len() - 1)];
        let gts_id = |schema: &Value| schema["$id"].as_str().unwrap_or_default().to_owned();
        match outcome {
            Registration::Unreachable => Err(CanonicalError::service_unavailable().create()),
            Registration::Stalled => std::future::pending().await,
            Registration::Accept => Ok(entities
                .iter()
                .map(|schema| RegisterResult::Ok {
                    gts_id: gts_id(schema),
                })
                .collect()),
            Registration::Reject => Ok(entities
                .iter()
                .map(|schema| RegisterResult::Err {
                    gts_id: Some(gts_id(schema)),
                    error: CanonicalError::internal("conflicting schema").create(),
                })
                .collect()),
        }
    }
    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_type_schemas(type_schemas).await
    }
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema(type_id).await
    }
    async fn get_type_schema_by_uuid(&self, id: Uuid) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema_by_uuid(id).await
    }
    async fn get_type_schemas(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas(ids).await
    }
    async fn get_type_schemas_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas_by_uuid(ids).await
    }
    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.inner.list_type_schemas(query).await
    }
    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.inner.register_instances(instances).await
    }
    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance(id).await
    }
    async fn get_instance_by_uuid(&self, id: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance_by_uuid(id).await
    }
    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances(ids).await
    }
    async fn get_instances_by_uuid(
        &self,
        ids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances_by_uuid(ids).await
    }
    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.inner.list_instances(query).await
    }
}

/// Starts the task over `registry` (not yet registered) and a recording
/// broker.
fn start_unregistered(
    registry: Arc<ScriptedRegistry>,
) -> (
    QueuePublisher,
    Arc<FakeBroker>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let metrics = Arc::new(AdmissionControlMetrics::global());
    let (publisher, rx) = QueuePublisher::new(8, Arc::clone(&metrics));
    let hub = Arc::new(ClientHub::new());
    let broker = Arc::new(FakeBroker::default());
    hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        rx,
        hub,
        registry,
        gate_identity().unwrap(),
        false,
        metrics,
        cancel.clone(),
    ));
    (publisher, broker, cancel, task)
}

#[tokio::test(start_paused = true)]
async fn registration_is_retried_until_it_succeeds_and_events_still_flow() {
    let registry = ScriptedRegistry::new(vec![
        Registration::Unreachable,
        Registration::Unreachable,
        Registration::Accept,
    ]);
    let (publisher, broker, cancel, task) = start_unregistered(Arc::clone(&registry));

    // Queued while registration is still being retried.
    publisher.emit(envelope(), event());
    tokio::time::sleep(super::REGISTRATION_RETRY * 10).await;
    assert_eq!(registry.calls(), 3, "retried twice, then stopped");
    assert_eq!(broker.published.lock().len(), 1);

    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_rejected_registration_is_not_retried() {
    let registry = ScriptedRegistry::new(vec![Registration::Unreachable, Registration::Reject]);
    let (_publisher, _broker, cancel, task) = start_unregistered(Arc::clone(&registry));
    tokio::time::sleep(super::REGISTRATION_RETRY * 10).await;
    assert_eq!(registry.calls(), 2);
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn events_queued_at_shutdown_are_counted_as_dropped() {
    let (metrics, exporter, provider) = recording();
    let (publisher, rx) = QueuePublisher::new(8, Arc::clone(&metrics));
    for _ in 0..3 {
        publisher.emit(envelope(), event());
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    run(
        rx,
        Arc::new(ClientHub::new()),
        Arc::new(MockTypesRegistryClient::new()),
        gate_identity().unwrap(),
        true,
        metrics,
        cancel,
    )
    .await;
    assert_eq!(dropped(&exporter, &provider), 3);
}

#[tokio::test(start_paused = true)]
async fn a_stalled_broker_neither_blocks_the_task_nor_shutdown() {
    let (metrics, exporter, provider) = recording();
    let (publisher, rx) = QueuePublisher::new(8, Arc::clone(&metrics));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn EventBrokerApi>(Arc::new(FakeBroker {
        stalled: true,
        ..FakeBroker::default()
    }) as Arc<dyn EventBrokerApi>);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        rx,
        hub,
        Arc::new(MockTypesRegistryClient::new()),
        gate_identity().unwrap(),
        true,
        metrics,
        cancel.clone(),
    ));

    // The publication times out and is counted.
    publisher.emit(envelope(), event());
    tokio::time::sleep(super::PUBLISH_TIMEOUT * 2).await;
    assert_eq!(dropped(&exporter, &provider), 1);

    // Cancellation interrupts a publication in flight.
    publisher.emit(envelope(), event());
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(dropped(&exporter, &provider), 2);
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_never_answers_counts_as_unreachable() {
    let registry = ScriptedRegistry::new(vec![Registration::Stalled]);
    assert!(!super::register_event_type(registry.as_ref()).await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn a_stalled_registration_times_out_is_retried_and_events_still_flow() {
    let registry = ScriptedRegistry::new(vec![Registration::Stalled, Registration::Accept]);
    let (publisher, broker, cancel, task) = start_unregistered(Arc::clone(&registry));

    publisher.emit(envelope(), event());
    tokio::time::sleep((super::REGISTRATION_TIMEOUT + super::REGISTRATION_RETRY) * 4).await;
    assert_eq!(
        registry.calls(),
        2,
        "the stalled call timed out and was retried once"
    );
    assert_eq!(broker.published.lock().len(), 1);

    cancel.cancel();
    task.await.unwrap();
}
