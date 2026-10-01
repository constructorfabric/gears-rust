//! `LeasedMessageHandler` draining the ingest outbox. Leases one row ->
//! decodes its `IngestPayload` (event, topic, partition-key value; the backend
//! picks the partition) -> resolves the topic's backend -> `Backend::append`s
//! the event under the fence keyed by `(outbox identity, message partition)`
//! with the message's seq -> on success, publishes a `domain::notify` wake-up for each
//! partition the backend reports it wrote.
//!
//! The backend is always called outside any transaction;
//! `LeasedMessageHandler::handle`'s signature, with no DB handle, makes that
//! structural.
//!
//! Every backend error is retried: nothing an ingest outbox holds is
//! dead-lettered for a backend failure (feature 0007 §3.1), so a message that
//! keeps failing stalls its outbox partition visibly instead of being lost. A
//! retry of a message the backend already stored is answered `Duplicate` by
//! the fence and acknowledged.

use std::sync::Arc;

use async_trait::async_trait;
use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::{PutRequest, Ttl};
use event_broker_sdk::backend::{AppendRequest, Appended, Fence, FenceSeq, Flush, KeyedEvent};
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use tracing::{error, warn};

use crate::domain::backend::{BackendResolver, to_sdk_event};
use crate::domain::notify::notification_key;
use crate::domain::outbox::{IngestPayload, OutboxId, fence_key};
use crate::domain::specification::SpecificationManager;

/// How long a notification entry lives in `ClusterCacheV1` before expiring.
/// Only matters for backend cleanup, not for whether a watcher observes it -
/// `watch_prefix` fires on the write itself, not on the entry's continued
/// existence - so this just needs to be "long enough that a laggy watcher
/// resubscribing has a moment to still see it," not tuned to any consumer
/// timing.
const NOTIFICATION_TTL: std::time::Duration = std::time::Duration::from_secs(10);

pub struct IngestOutboxHandler {
    spec_manager: Arc<dyn SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    cache: ClusterCacheV1,
    /// This outbox's identity: the high half of every fence key it appends under.
    outbox: OutboxId,
}

impl IngestOutboxHandler {
    #[must_use]
    pub fn new(
        spec_manager: Arc<dyn SpecificationManager>,
        backend_resolver: Arc<dyn BackendResolver>,
        cache: ClusterCacheV1,
        outbox: OutboxId,
    ) -> Self {
        Self {
            spec_manager,
            backend_resolver,
            cache,
            outbox,
        }
    }

    /// Best-effort: a failure here (surrogate-id resolution or the cache
    /// write itself) is logged and swallowed, never turned into a `Retry`/
    /// `Reject` - the event is already durably persisted by this point, and
    /// the notification is only a wake-up hint (design.md D6), not part of
    /// this handler's durability contract. A missed notification costs
    /// delivery's stream loop one extra heartbeat-interval wait, not a
    /// correctness bug.
    async fn notify(&self, topic_id: &toolkit_gts::GtsInstanceId, partition: i32) {
        // Derived from the identifier, not looked up: the key has to be the
        // same on every instance that writes or watches it, and a value this
        // instance's database assigned is not that.
        let Some(key) = notification_key(topic_id, partition) else {
            warn!(topic = %topic_id, "topic identifier does not derive a notification key");
            return;
        };
        if let Err(err) = self
            .cache
            .put(PutRequest {
                key: &key,
                value: b"",
                ttl: Ttl::Of(NOTIFICATION_TTL),
            })
            .await
        {
            warn!(topic = %topic_id, partition, %err, "publishing delivery notification failed");
        }
    }
}

#[async_trait]
impl LeasedMessageHandler for IngestOutboxHandler {
    #[tracing::instrument(name = "worker", skip_all, fields(worker = "ingest_outbox"))]
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let payload = match serde_json::from_slice::<IngestPayload>(&msg.payload) {
            Ok(payload) => payload,
            Err(e) => {
                error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    payload_len = msg.payload.len(),
                    "ingest outbox payload deserialization failed: {e}"
                );
                return MessageResult::Reject(format!("deserialization failed: {e}"));
            }
        };

        // `get_topic` returning `None` conflates two cases: the topic was
        // genuinely deregistered, and the local spec-cache read itself failed
        // (a transient SQLite error is swallowed into `None` in
        // `infra::specification`). Both are dead-lettered (`Reject`), a known
        // gap for the transient case.
        let Some(topic) = self.spec_manager.get_topic(&payload.topic).await else {
            error!(topic = %payload.topic, "ingest outbox payload references a topic that is no longer registered");
            return MessageResult::Reject(format!("topic '{}' is not registered", payload.topic));
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
            Ok(Appended::Heads(heads)) => {
                for head in heads {
                    self.notify(&topic.id, head.at.partition.value().cast_signed())
                        .await;
                }
                MessageResult::Ok
            }
            // `Accepted`: stored, positions arrive through the backend's watch;
            // `Duplicate`: stored by an earlier attempt of this message.
            Ok(Appended::Accepted | Appended::Duplicate) => MessageResult::Ok,
            Err(e) => {
                warn!(
                    topic = %topic.id,
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "ingest outbox append failed - will retry"
                );
                MessageResult::Retry
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use event_broker_sdk::Sequence;
    use event_broker_sdk::backend::{
        Backend, LogClaim, LogId, Replacing, Retention, TopicPartition, TopicSpec, Wait,
    };
    use memory_event_broker_plugin::MemoryBackend;
    use toolkit_db::DBProvider;
    use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
    use uuid::Uuid;

    use event_broker_sdk::backend::Partition;

    use super::*;
    use crate::domain::backend::SingleBackendResolver;
    use crate::domain::model::{Event, Meta};
    use crate::domain::outbox::INGEST_PAYLOAD_TYPE;
    use crate::infra::specification::TypesRegistrySpecificationManager;
    use crate::infra::storage::migrations::Migrator;

    const TOPIC_ID: &str = "gts.cf.core.events.topic.v1~example.eb.ingestoutbox.topic.v1";
    const OUTBOX: OutboxId = OutboxId::new(0x0123_4567_89ab_cdef);

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
    async fn test_handler(provisioned: bool) -> (IngestOutboxHandler, MemoryBackend) {
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
        let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
        (
            IngestOutboxHandler::new(spec_manager, resolver, cluster.cache, OUTBOX),
            backend,
        )
    }

    fn test_event(sequence: i64) -> Event {
        Event {
            id: Uuid::new_v4(),
            r#type: crate::test_support::event_type_id(
                "gts.cf.core.events.event.v1~example.eb.ingestoutbox.event.v1~",
            ),
            topic: topic(),
            tenant_id: Uuid::new_v4(),
            source: "test".to_owned(),
            subject: "subject".to_owned(),
            subject_type: gts::GtsTypeId::new("gts.example.eb.ingestoutbox.subject.v1~"),
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
            created_at: chrono::Utc::now(),
            attempts: 0,
        }
    }

    /// A redelivery of the exact same outbox row - a crash between `append`
    /// succeeding and the row being marked done - carries the same fence, so
    /// the backend answers `Duplicate`, the row is acknowledged, and the event
    /// is stored once.
    #[tokio::test]
    async fn redelivery_after_append_is_a_duplicate() {
        let (handler, backend) = test_handler(true).await;
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
    }

    #[tokio::test]
    async fn a_topic_not_provisioned_is_retried() {
        let (handler, _backend) = test_handler(false).await;
        let result = handler.handle(&test_message(&test_event(1))).await;
        assert!(matches!(result, MessageResult::Retry), "{result:?}");
    }

    #[tokio::test]
    async fn malformed_payload_is_rejected() {
        let (handler, _backend) = test_handler(true).await;
        let msg = OutboxMessage {
            partition_id: 0,
            seq: 1,
            payload: b"not json".to_vec(),
            payload_type: INGEST_PAYLOAD_TYPE.to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0,
        };
        let result = handler.handle(&msg).await;
        assert!(matches!(result, MessageResult::Reject(_)), "{result:?}");
    }

    #[tokio::test]
    async fn unregistered_topic_is_rejected() {
        let (handler, _backend) = test_handler(true).await;
        let mut event = test_event(1);
        event.topic = toolkit_gts::GtsInstanceId::try_new(
            "gts.cf.core.events.topic.v1~example.eb.ingestoutbox.unregistered.v1",
        )
        .unwrap();
        let result = handler.handle(&test_message(&event)).await;
        assert!(matches!(result, MessageResult::Reject(_)), "{result:?}");
    }
}
