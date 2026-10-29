//! Orders-owned real-registry broker fixture, assembled only from public APIs.
use std::{sync::Arc, time::Duration};

use authz_resolver_sdk::PolicyEnforcer;
use event_broker::{
    config::{BatchConfig, EventBrokerConfig, LoaderConfig, StreamingConfig},
    domain::{
        backend::SingleBackendResolver, consumer_group_coordinator::ConsumerGroupCoordinator,
        local_broker::LocalBroker, outbox::INGEST_QUEUE_NAME, specification::SpecificationManager,
        streaming::lease::InProcessStreamLeases,
    },
    infra::{
        specification::{TypesRegistrySpecificationManager, bulk_load},
        storage::Storage,
        wiring::{build_handler_state, build_topic_manager, loader_future},
        workers::IngestOutboxHandler,
    },
};
use event_broker_sdk::{EventBrokerApi, EventBrokerBackend};
use sqlite_event_broker_plugin::{EventLogPath, SqliteEventBackend};
use toolkit_db::{
    DBProvider,
    outbox::{Outbox, OutboxHandle, Partitions},
};
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

pub struct EventBrokerHarness {
    broker: Arc<dyn EventBrokerApi>,
    backend: Arc<dyn EventBrokerBackend>,
    storage: Arc<Storage>,
    ctx: SecurityContext,
    // Dropping the handle cancels the ingest workers, so retain it for the fixture lifetime.
    _outbox: OutboxHandle,
    cancel: tokio_util::sync::CancellationToken,
    loader: tokio::task::JoinHandle<()>,
    db_path: std::path::PathBuf,
}

impl Drop for EventBrokerHarness {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.loader.abort();
        // SQLite connections can outlive this object briefly while workers stop. On Unix
        // removing the open file is safe; on other platforms cleanup is best effort.
        drop(std::fs::remove_file(&self.db_path));
    }
}

impl EventBrokerHarness {
    pub fn builder() -> EventBrokerHarnessBuilder {
        EventBrokerHarnessBuilder::default()
    }
    pub fn broker(&self) -> Arc<dyn EventBrokerApi> {
        self.broker.clone()
    }
    pub fn backend(&self) -> &Arc<dyn EventBrokerBackend> {
        &self.backend
    }
    pub fn security_context(&self) -> &SecurityContext {
        &self.ctx
    }
    pub async fn forget_producer(&self, id: Uuid) {
        self.storage
            .delete_producer_registration(id)
            .await
            .expect("delete producer registration");
    }
}

#[derive(Default)]
pub struct EventBrokerHarnessBuilder {
    registry: Option<Arc<dyn TypesRegistryClient>>,
    topics: serde_json::Map<String, serde_json::Value>,
    policy: Option<PolicyEnforcer>,
}
impl EventBrokerHarnessBuilder {
    pub fn with_types_registry_client(
        mut self,
        client: Arc<dyn TypesRegistryClient>,
        topics: &[(&str, u32)],
    ) -> Self {
        self.registry = Some(client);
        for (topic, partitions) in topics {
            self.topics.insert(
                (*topic).into(),
                serde_json::json!({"partitions": partitions}),
            );
        }
        self
    }
    pub fn with_policy_enforcer(mut self, policy: PolicyEnforcer) -> Self {
        self.policy = Some(policy);
        self
    }
    pub async fn build(self) -> EventBrokerHarness {
        let db_path = std::env::temp_dir().join(format!("orders-broker-{}.db", Uuid::new_v4()));
        let db = toolkit_db::connect_db(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("broker metadata database");
        toolkit_db::migration_runner::run_migrations_for_testing(&db,
            <event_broker::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations()
        ).await.expect("broker metadata migrations");
        let db = Arc::new(DBProvider::new(db));
        let config: EventBrokerConfig = serde_json::from_value(serde_json::json!({
            "mode": "standalone", "default_storage_backend": "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~",
            "topics": self.topics
        })).expect("broker deployment config");
        // The negative fixture deliberately leaves the metadata cache empty. All
        // positive fixtures load the supplied, real committed registry unchanged.
        if let Some(registry) = self.registry {
            bulk_load(&registry, &db, &config)
                .await
                .expect("load real committed registry");
        }
        let spec: Arc<dyn SpecificationManager> =
            Arc::new(TypesRegistrySpecificationManager::new(db.clone()));
        let (_hub, cluster) = event_broker::test_support::standalone_event_broker_cluster().await;
        let storage = Arc::new(Storage::new(db.clone(), spec.clone(), Uuid::new_v4()));
        storage.set_cache(cluster.cache.clone());
        let backend: Arc<dyn EventBrokerBackend> = Arc::new(
            SqliteEventBackend::open(&EventLogPath::InMemory)
                .await
                .expect("event log"),
        );
        let resolver: Arc<dyn event_broker::domain::backend::BackendResolver> =
            Arc::new(SingleBackendResolver::new(backend.clone()));
        let outbox = Outbox::builder(db.db())
            .queue(INGEST_QUEUE_NAME, Partitions::of(4))
            .leased(IngestOutboxHandler::new(
                spec.clone(),
                resolver.clone(),
                cluster.cache,
            ))
            .start()
            .await
            .expect("ingest outbox");
        storage.set_outbox(outbox.outbox().clone());
        let loader_config = LoaderConfig {
            tick_ms: 2,
            ..Default::default()
        };
        let topics = build_topic_manager(&loader_config);
        let cancel = tokio_util::sync::CancellationToken::new();
        let loader = tokio::spawn(loader_future(
            &loader_config,
            topics.clone(),
            spec.clone(),
            resolver.clone(),
            cancel.clone(),
        ));
        let groups = Arc::new(ConsumerGroupCoordinator::new(Duration::from_secs(30)));
        let policy = self.policy.unwrap_or_else(|| {
            PolicyEnforcer::new(Arc::new(
                event_broker::test_support::authz_doubles::AllowAllAuthZ,
            ))
        });
        let state = build_handler_state(
            storage.clone(),
            policy,
            spec,
            resolver,
            topics,
            groups,
            Arc::new(InProcessStreamLeases::new()),
            BatchConfig::default(),
            StreamingConfig::default(),
            Duration::from_secs(1),
        );
        EventBrokerHarness {
            broker: Arc::new(LocalBroker::new(state.ingest, state.delivery)),
            backend,
            storage,
            ctx: SecurityContext::builder()
                .subject_tenant_id(Uuid::new_v4())
                .subject_id(Uuid::new_v4())
                .build()
                .expect("broker read context"),
            _outbox: outbox,
            cancel,
            loader,
            db_path,
        }
    }
}

#[tokio::test]
async fn local_harness_loads_the_committed_orders_registry() {
    let harness = EventBrokerHarness::builder()
        .with_types_registry_client(
            super::registry_client(),
            &[(crate::infra::events::TOPIC, 4)],
        )
        .build()
        .await;
    let broker = harness.broker();
    let topics = broker
        .list_topics(harness.security_context())
        .await
        .unwrap();
    assert!(
        topics
            .iter()
            .any(|t| t.id.as_ref() == crate::infra::events::TOPIC)
    );
    let types = broker
        .list_event_types(harness.security_context())
        .await
        .unwrap();
    for id in crate::infra::events::payload::EVENT_TYPE_IDS {
        assert!(types.iter().any(|t| t.id.as_ref() == id), "missing {id}");
    }
}

#[tokio::test]
async fn local_harness_faults_preserve_real_appends_and_schema_validation() {
    use super::faults::{DeliveredOutcome, FaultInjectingBroker, TransientFault};
    use event_broker_sdk::{Event, EventBrokerError, IngestOutcome, ProducerMode, Sequence};
    let harness = EventBrokerHarness::builder()
        .with_types_registry_client(
            super::registry_client(),
            &[(crate::infra::events::TOPIC, 1)],
        )
        .build()
        .await;
    let client = FaultInjectingBroker::new(harness.broker());
    let ctx = harness.security_context();
    let producer = client
        .register_producer(ctx, ProducerMode::Chained, "orders-harness-test")
        .await
        .unwrap();
    client.fail_cursor_reads([TransientFault::Transport, TransientFault::RateLimited]);
    assert!(matches!(
        client.get_producer_cursors(ctx, producer).await,
        Err(EventBrokerError::Transport(_))
    ));
    assert!(matches!(
        client.get_producer_cursors(ctx, producer).await,
        Err(EventBrokerError::RateLimited { .. })
    ));
    assert!(
        client
            .get_producer_cursors(ctx, producer)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(client.cursor_reads(), 3);
    let data: serde_json::Value =
        serde_json::from_slice(&super::encoded_max_events(Uuid::new_v4())[0].1).unwrap();
    let mut event = Event {
        id: Uuid::new_v4(),
        type_id: event_broker_sdk::GtsTypeId::try_new(
            crate::infra::events::payload::EVENT_TYPE_IDS[0],
        )
        .unwrap(),
        tenant_id: ctx.subject_tenant_id(),
        source: "bss-orders".into(),
        subject: data["orderId"].as_str().unwrap().into(),
        subject_type: event_broker_sdk::GtsTypeId::try_new(
            crate::infra::events::ORDER_SUBJECT_TYPE,
        )
        .unwrap(),
        occurred_at: serde_json::from_value(serde_json::json!("2026-10-01T00:00:00Z")).unwrap(),
        trace_parent: None,
        data: Some(data),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: Some(event_broker_sdk::models::ProducerMeta {
            version: 1,
            producer_id: Some(producer.0),
            previous: Some(0),
            sequence: Some(1),
            partition_hint: Some(0),
        }),
    };
    // An invalid payload must be refused by the actual loaded schema.
    let valid_data = event.data.take();
    event.data = Some(serde_json::json!({}));
    assert!(client.publish(ctx, &event).await.is_err());
    event.data = valid_data;
    client.fail_publishes([TransientFault::RateLimited]);
    assert!(matches!(
        client.publish(ctx, &event).await,
        Err(EventBrokerError::RateLimited { .. })
    ));
    client.lose_next_ack_then_disconnect();
    assert!(matches!(
        client.publish(ctx, &event).await,
        Err(EventBrokerError::Transport(_))
    ));
    assert!(client.is_disconnected());
    assert!(matches!(
        client.publish(ctx, &event).await,
        Err(EventBrokerError::Transport(_))
    ));
    client.reconnect();
    assert_eq!(
        client.publish(ctx, &event).await.unwrap(),
        IngestOutcome::Duplicate
    );
    let log = client.delivered_publishes();
    assert_eq!(log.len(), 3);
    assert!(matches!(log[0].outcome, DeliveredOutcome::Refused(_)));
    assert!(log[1].ack_lost && log[1].outcome == DeliveredOutcome::Accepted);
    assert_eq!(log[2].outcome, DeliveredOutcome::Duplicate);
    assert_eq!(log[1].event_id, log[2].event_id);
    let stored = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = harness
                .backend()
                .read(ctx, crate::infra::events::TOPIC, 0, Sequence::NONE, 10)
                .await
                .unwrap();
            if !events.is_empty() {
                break events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, event.id);
    harness.forget_producer(producer.0).await;
    assert!(matches!(
        client.get_producer_cursors(ctx, producer).await,
        Err(EventBrokerError::UnknownProducer { .. })
    ));
}
