//! `LeasedMessageHandler` draining the ingest outbox. Leases one row ->
//! decodes its `IngestPayload` (event, topic, partition-key value; the backend
//! picks the partition) -> resolves the topic's backend -> `Backend::append`s
//! the event under the fence keyed by `(outbox identity, message partition)`
//! with the message's seq. Delivery learns of the new head through the
//! backend's own watch (`infra::loader::heads`), not from here.
//!
//! The backend is always called outside any transaction;
//! `LeasedMessageHandler::handle`'s signature, with no DB handle, makes that
//! structural.
//!
//! Nothing an ingest outbox holds is dead-lettered (feature 0007 §4.1): a
//! backend error, a payload that does not decode and a topic the registry does
//! not hold are all retried, so a message that keeps failing stalls its outbox
//! partition visibly - in `infra::metrics` and the log - instead of being
//! lost. A retry of a message the backend already stored is answered
//! `Duplicate` by the fence and acknowledged.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::backend::{AppendRequest, Appended, Fence, FenceSeq, Flush, KeyedEvent};
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use tracing::{error, warn};

use crate::domain::backend::{BackendResolver, to_sdk_event};
use crate::domain::outbox::{IngestPayload, OutboxId, fence_key};
use crate::domain::specification::SpecificationManager;
use crate::infra::metrics::{IngestMetrics, NO_BACKEND, RetryCause};

pub struct IngestOutboxHandler {
    spec_manager: Arc<dyn SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    /// This outbox's identity: the high half of every fence key it appends under.
    outbox: OutboxId,
    metrics: Arc<IngestMetrics>,
}

impl IngestOutboxHandler {
    #[must_use]
    pub fn new(
        spec_manager: Arc<dyn SpecificationManager>,
        backend_resolver: Arc<dyn BackendResolver>,
        outbox: OutboxId,
        metrics: Arc<IngestMetrics>,
    ) -> Self {
        Self {
            spec_manager,
            backend_resolver,
            outbox,
            metrics,
        }
    }

    /// Appends the message's event; on failure, the backend label and the
    /// cause of the retry.
    async fn append(&self, msg: &OutboxMessage) -> Result<(), (String, RetryCause)> {
        let payload = decode(msg)?;

        // `get_topic` returning `None` conflates two cases: the topic was
        // genuinely deregistered, and the local spec-cache read itself failed
        // (a transient gear-database error is swallowed into `None` in
        // `infra::specification`). Both are retried: the second heals by
        // itself, and the first is the operator's to resolve.
        let Some(topic) = self.spec_manager.get_topic(&payload.topic).await else {
            error!(topic = %payload.topic, "ingest outbox payload references a topic that is not registered - will retry");
            return Err((NO_BACKEND.to_owned(), RetryCause::UnregisteredTopic));
        };

        let backend = self.backend_resolver.resolve(&topic);
        let events = [KeyedEvent {
            key: payload.key,
            event: to_sdk_event(&payload.event),
        }];
        let request = AppendRequest {
            fence: Fence {
                key: fence_key(self.outbox, msg.partition_id),
                seq: FenceSeq::new(msg.seq),
            },
            topic: &topic.id,
            events: &events,
            // `Prefer: wait` is answered `501`, so nothing waits on positions
            // being assigned.
            flush: Flush::Async,
        };
        match backend.append(&request).await {
            // `Heads` and `Accepted`: stored; `Duplicate`: stored by an earlier
            // attempt of this message.
            Ok(Appended::Heads(_) | Appended::Accepted | Appended::Duplicate) => Ok(()),
            Err(e) => {
                warn!(
                    topic = %topic.id,
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "ingest outbox append failed - will retry"
                );
                Err((
                    topic.settings.backend().value().to_string(),
                    RetryCause::Append(e),
                ))
            }
        }
    }
}

fn decode(msg: &OutboxMessage) -> Result<IngestPayload, (String, RetryCause)> {
    serde_json::from_slice::<IngestPayload>(&msg.payload).map_err(|e| {
        error!(
            partition_id = msg.partition_id,
            seq = msg.seq,
            payload_len = msg.payload.len(),
            "ingest outbox payload does not decode - will retry: {e}"
        );
        (NO_BACKEND.to_owned(), RetryCause::UndecodablePayload)
    })
}

#[async_trait]
impl LeasedMessageHandler for IngestOutboxHandler {
    #[tracing::instrument(name = "worker", skip_all, fields(worker = "ingest_outbox"))]
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        self.metrics.handling(msg.partition_id, msg.created_at);
        match self.append(msg).await {
            Ok(()) => {
                self.metrics.handled(msg.partition_id);
                MessageResult::Ok
            }
            Err((backend, cause)) => {
                self.metrics.retried(msg.partition_id, &backend, &cause);
                MessageResult::Retry
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Utc};
    use event_broker_sdk::Sequence;
    use event_broker_sdk::backend::{
        Backend, LogClaim, LogId, Replacing, Retention, TopicPartition, TopicSpec, Wait,
    };
    use memory_event_broker_plugin::MemoryBackend;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use toolkit_db::DBProvider;
    use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
    use uuid::Uuid;

    use event_broker_sdk::backend::Partition;

    use super::*;
    use crate::domain::model::{Event, Meta};
    use crate::domain::outbox::{INGEST_PAYLOAD_TYPE, INGEST_QUEUE_NAME};
    use crate::infra::metrics::SCOPE;
    use crate::infra::specification::TypesRegistrySpecificationManager;
    use crate::infra::storage::migrations::Migrator;
    use crate::test_support::SingleBackendResolver;

    const TOPIC_ID: &str =
        toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.ingestoutbox.topic.v1");
    const OUTBOX: OutboxId = OutboxId::new(0x0123_4567_89ab_cdef);
    const RETRIES: &str = "evbk_ingest_append_retries_total";
    const OLDEST_AGE: &str = "evbk_ingest_outbox_oldest_age_seconds";

    /// When every test message was enqueued.
    fn created_at() -> DateTime<Utc> {
        "2026-10-29T12:00:00Z".parse().unwrap()
    }

    /// The clock the metrics read: 90 s after [`created_at`].
    fn now() -> DateTime<Utc> {
        "2026-10-29T12:01:30Z".parse().unwrap()
    }

    /// One data point: its labels, sorted by key, and its value.
    type Point = (Vec<(String, String)>, f64);

    /// The handler's metrics, read back from an in-memory exporter.
    struct Recorder {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
    }

    impl Recorder {
        fn new() -> (Self, Arc<IngestMetrics>) {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            let metrics = IngestMetrics::new(&provider.meter(SCOPE), INGEST_QUEUE_NAME, now);
            (Self { provider, exporter }, Arc::new(metrics))
        }

        /// Every data point of `name` as its labels and value, sorted.
        fn points(&self, name: &str) -> Vec<Point> {
            self.provider.force_flush().unwrap();
            let exported = self.exporter.get_finished_metrics().unwrap();
            let Some(latest) = exported.last() else {
                return Vec::new();
            };
            let mut points = Vec::new();
            for metric in latest
                .scope_metrics()
                .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            {
                if metric.name() != name {
                    continue;
                }
                match metric.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        for point in sum.data_points() {
                            #[allow(clippy::cast_precision_loss)]
                            points.push((labels(point.attributes()), point.value() as f64));
                        }
                    }
                    AggregatedMetrics::F64(MetricData::Gauge(gauge)) => {
                        for point in gauge.data_points() {
                            points.push((labels(point.attributes()), point.value()));
                        }
                    }
                    other => panic!("{name} has an unexpected shape: {other:?}"),
                }
            }
            points.sort_by(|a, b| a.0.cmp(&b.0));
            points
        }
    }

    fn labels<'a>(
        attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>,
    ) -> Vec<(String, String)> {
        let mut labels: Vec<(String, String)> = attributes
            .map(|kv| (kv.key.to_string(), kv.value.to_string()))
            .collect();
        labels.sort();
        labels
    }

    fn label(key: &str, value: &str) -> (String, String) {
        (key.to_owned(), value.to_owned())
    }

    async fn test_db() -> Arc<DBProvider<toolkit_db::DbError>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cf-eb-ingestoutbox-test-{}.db",
            Uuid::now_v7().simple()
        ));
        let mut file = path.to_string_lossy().replace('\\', "/");
        if !file.starts_with('/') {
            file.insert(0, '/');
        }
        let dsn = format!("sqlite://{file}?mode=rwc");
        let opts = toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = toolkit_db::connect_db(&dsn, opts)
            .await
            .expect("connect sqlite");
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            <Migrator as sea_orm_migration::MigratorTrait>::migrations(),
        )
        .await
        .expect("migrations");
        Arc::new(DBProvider::new(db))
    }

    fn topic() -> toolkit_gts::GtsInstanceId {
        toolkit_gts::GtsInstanceId::try_new(TOPIC_ID).unwrap()
    }

    /// The handler over a registered topic; `provisioned` decides whether the
    /// backend holds the topic's log.
    async fn test_handler(provisioned: bool) -> (IngestOutboxHandler, MemoryBackend, Recorder) {
        let db = test_db().await;
        let instance = make_test_instance(
            TOPIC_ID,
            serde_json::json!({
                "id": TOPIC_ID,
                "description": "a topic this test drains the outbox into",
            }),
        );
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
        crate::infra::specification::bulk_load(
            &client,
            &db,
            &crate::test_support::StaticTypesRegistry::empty_config(),
        )
        .await
        .expect("bulk_load");
        let spec_manager: Arc<dyn SpecificationManager> =
            Arc::new(TypesRegistrySpecificationManager::new(Arc::clone(&db)));
        let backend = MemoryBackend::new();
        if provisioned {
            backend
                .provision(
                    &TopicSpec {
                        topic: topic(),
                        partitions: 1,
                        retention: Retention::default(),
                    },
                    &LogClaim {
                        replacing: Replacing::Nothing,
                        new: LogId::from_uuid(Uuid::from_u128(1)),
                    },
                )
                .await
                .expect("provision");
        }
        let resolver = Arc::new(SingleBackendResolver::new(Arc::new(backend.clone())));
        let (recorder, metrics) = Recorder::new();
        (
            IngestOutboxHandler::new(spec_manager, resolver, OUTBOX, metrics),
            backend,
            recorder,
        )
    }

    fn test_event(sequence: i64) -> Event {
        Event {
            id: Uuid::new_v4(),
            r#type: crate::test_support::event_type_id(toolkit_gts::gts_id!(
                "cf.core.events.event.v1~example.eb.ingestoutbox.event.v1~"
            )),
            topic: topic(),
            tenant_id: Uuid::new_v4(),
            source: "test".to_owned(),
            subject: "subject".to_owned(),
            subject_type: gts::GtsTypeId::new(toolkit_gts::gts_id!(
                "example.eb.ingestoutbox.subject.v1~"
            )),
            occurred_at: chrono::Utc::now(),
            trace_parent: None,
            data: serde_json::json!({"k": "v"}),
            meta: Some(Meta {
                version: 1,
                producer_id: Uuid::new_v4(),
                previous: Some(0),
                sequence,
            }),
            partition: None,
            sequence: None,
            sequence_time: None,
        }
    }

    fn test_message(event: &Event) -> OutboxMessage {
        OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload: serde_json::to_vec(&IngestPayload {
                topic: event.topic.clone(),
                key: "key".to_owned(),
                event: event.clone(),
            })
            .expect("serialize payload"),
            payload_type: INGEST_PAYLOAD_TYPE.to_owned(),
            created_at: created_at(),
            attempts: 0,
        }
    }

    /// A redelivery of the exact same outbox row - a crash between `append`
    /// succeeding and the row being marked done - carries the same fence, so
    /// the backend answers `Duplicate`, the row is acknowledged, and the event
    /// is stored once.
    #[tokio::test]
    async fn redelivery_after_append_is_a_duplicate() {
        let (handler, backend, recorder) = test_handler(true).await;
        let event = test_event(1);
        let msg = test_message(&event);

        assert!(matches!(handler.handle(&msg).await, MessageResult::Ok));
        assert!(matches!(handler.handle(&msg).await, MessageResult::Ok));

        let stored = backend
            .read(
                &TopicPartition::new(topic(), Partition::new(0)),
                Sequence::NONE,
                100,
                Wait::No,
            )
            .await
            .expect("read must succeed");
        let stored: Vec<_> = stored
            .iter()
            .map(|e| (e.id, e.partition, e.sequence))
            .collect();
        assert_eq!(
            stored,
            vec![(event.id, Some(0_u32), Some(Sequence::assigned(1)))]
        );
        assert_eq!(recorder.points(RETRIES), vec![]);
        assert_eq!(
            recorder.points(OLDEST_AGE),
            vec![(
                vec![label("outbox", "default"), label("partition", "0")],
                0.0
            )]
        );
    }

    /// Not provisioned yet: retried, counted against the topic's backend, and
    /// the message's age reported while it waits.
    #[tokio::test]
    async fn a_topic_not_provisioned_is_retried() {
        let (handler, _backend, recorder) = test_handler(false).await;
        let result = handler.handle(&test_message(&test_event(1))).await;
        assert!(matches!(result, MessageResult::Retry), "{result:?}");
        assert_eq!(
            recorder.points(RETRIES),
            vec![(
                vec![
                    label("backend", "main"),
                    label("error", "not_provisioned"),
                    label("outbox", "default"),
                    label("partition", "0"),
                ],
                1.0
            )]
        );
        assert_eq!(
            recorder.points(OLDEST_AGE),
            vec![(
                vec![label("outbox", "default"), label("partition", "0")],
                90.0
            )]
        );
    }

    /// A payload that does not decode is never dead-lettered: it stalls its
    /// outbox partition, visibly.
    #[tokio::test]
    async fn malformed_payload_is_retried() {
        let (handler, _backend, recorder) = test_handler(true).await;
        let msg = OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload: b"not json".to_vec(),
            payload_type: INGEST_PAYLOAD_TYPE.to_owned(),
            created_at: created_at(),
            attempts: 0,
        };
        let result = handler.handle(&msg).await;
        assert!(matches!(result, MessageResult::Retry), "{result:?}");
        assert_eq!(
            recorder.points(RETRIES),
            vec![(
                vec![
                    label("backend", "none"),
                    label("error", "undecodable_payload"),
                    label("outbox", "default"),
                    label("partition", "0"),
                ],
                1.0
            )]
        );
        assert_eq!(
            recorder.points(OLDEST_AGE),
            vec![(
                vec![label("outbox", "default"), label("partition", "0")],
                90.0
            )]
        );
    }

    /// A topic the registry does not hold is retried too: a failed registry
    /// read heals by itself, and a deregistered topic is the operator's call.
    #[tokio::test]
    async fn unregistered_topic_is_retried() {
        let (handler, _backend, recorder) = test_handler(true).await;
        let mut event = test_event(1);
        event.topic = toolkit_gts::GtsInstanceId::try_new(toolkit_gts::gts_id!(
            "cf.core.events.topic.v1~example.eb.ingestoutbox.unregistered.v1"
        ))
        .unwrap();
        let result = handler.handle(&test_message(&event)).await;
        assert!(matches!(result, MessageResult::Retry), "{result:?}");
        assert_eq!(
            recorder.points(RETRIES),
            vec![(
                vec![
                    label("backend", "none"),
                    label("error", "unregistered_topic"),
                    label("outbox", "default"),
                    label("partition", "0"),
                ],
                1.0
            )]
        );
        assert_eq!(
            recorder.points(OLDEST_AGE),
            vec![(
                vec![label("outbox", "default"), label("partition", "0")],
                90.0
            )]
        );
    }
}
