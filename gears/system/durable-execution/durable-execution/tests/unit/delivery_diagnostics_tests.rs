//! Retry diagnostics must explain the failed phase without exposing payloads or errors.
use super::*;
use std::sync::Arc;
use tracing::instrument::WithSubscriber;

type Fields = BTreeMap<String, String>;
#[derive(Clone, Default)]
struct Capture(Arc<parking_lot::Mutex<Vec<Fields>>>);
struct Visitor(Fields);
impl tracing::field::Visit for Visitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
}
impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = Visitor(BTreeMap::new());
        event.record(&mut visitor);
        if visitor
            .0
            .get("message")
            .is_some_and(|message| message == "durable delivery retry")
        {
            self.0.lock().push(visitor.0);
        }
    }
}

async fn captured(future: impl std::future::Future<Output = MessageResult>) -> Fields {
    let capture = Capture::default();
    assert!(matches!(
        future.with_subscriber(capture.clone()).await,
        MessageResult::Retry
    ));
    let events = capture.0.lock();
    assert_eq!(events.len(), 1);
    events[0].clone()
}
fn message(id: uuid::Uuid, activity: &str) -> OutboxMessage {
    OutboxMessage {
        partition_id: 0,
        seq: 1,
        payload: serde_json::to_vec(&Delivery {
            run_id: id.to_string(),
            generation: 0,
            activity_id: activity.to_owned(),
        })
        .unwrap(),
        payload_type: PAYLOAD_TYPE.into(),
        created_at: Utc::now(),
        attempts: 0,
    }
}
fn handler(store: JournalStore, queue: Option<Queue>) -> DeliverToApalis {
    DeliverToApalis {
        store,
        queues: queue
            .map(|queue| [("durable-v1".into(), queue)].into())
            .unwrap_or_default(),
        config: Config::default(),
        #[cfg(feature = "integration")]
        probe: None,
    }
}
fn assert_fields(fields: &Fields, id: uuid::Uuid, phase: &str, category: &str, queue: &str) {
    assert_eq!(fields.get("run_id"), Some(&id.to_string()));
    assert_eq!(fields.get("generation").map(String::as_str), Some("0"));
    assert_eq!(fields.get("phase").map(String::as_str), Some(phase));
    assert_eq!(
        fields.get("error_category").map(String::as_str),
        Some(category)
    );
    assert_eq!(fields.get("queue").map(String::as_str), Some(queue));
    let encoded = format!("{fields:?}");
    for private in [
        "queue-secret",
        "private-input",
        "private-password",
        "postgresql://",
    ] {
        assert!(!encoded.contains(private), "diagnostic leaked {private}");
    }
}

struct Allowed;
#[async_trait]
impl crate::infra::authorization::WorkerAuthorization for Allowed {
    async fn scope(&self, _: &str) -> Result<toolkit_security::AccessScope, StoreError> {
        Ok(toolkit_security::AccessScope::allow_all())
    }
}
#[tokio::test]
async fn failed_journal_read_logs_safe_identity_and_phase() {
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    let store = JournalStore::new(Arc::new(toolkit_db::DBProvider::new(db)))
        .with_authorization(Arc::new(Allowed));
    let handler = handler(store, None);
    let id = uuid::Uuid::new_v4();
    let message = message(id, "postgresql://worker:private-password@database/journal");
    let fields = captured(handler.handle(&message)).await;
    assert_fields(&fields, id, "journal_read", "database", "<unresolved>");
    assert_eq!(
        fields.get("activity_id").map(String::as_str),
        Some("<invalid>")
    );
}

#[tokio::test]
async fn failed_queue_enqueue_logs_phase_and_category_without_backend_details() {
    use crate::infra::storage::repository::tests::{journal, store};
    let store = store().await;
    let mut journal = journal();
    journal.input = serde_json::json!({"secret": "private-input"});
    let id = journal.run.id.0;
    store
        .insert(toolkit_security::AccessScope::allow_all(), journal)
        .await
        .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://fixture:queue-secret@127.0.0.1/durable")
        .unwrap();
    pool.close().await;
    let queue = Queue::from_pool(pool, "durable-v1", 1);
    let handler = handler(store, Some(queue));
    let message = message(id, "one");
    let fields = captured(handler.handle(&message)).await;
    assert_fields(&fields, id, "enqueue", "queue_database", "durable-v1");
    assert_eq!(fields.get("activity_id").map(String::as_str), Some("one"));
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn failed_intent_ack_logs_its_phase_after_successful_queue_enqueue() {
    use crate::infra::storage::repository::tests::{isolated_url, journal, store_at};
    use sea_orm::ConnectionTrait;
    struct BreakAck(sea_orm::DatabaseConnection);
    #[async_trait]
    impl test_probe::Probe for BreakAck {
        async fn fail_at(&self, phase: test_probe::Phase) -> bool {
            if phase == test_probe::Phase::AfterEnqueue {
                // Keep the table present so search_path cannot fall back to public.
                self.0
                    .execute_unprepared(
                        "ALTER TABLE durable_outbox RENAME COLUMN delivered_at TO unavailable_delivered_at",
                    )
                    .await
                    .unwrap();
            }
            false
        }
    }
    let database = isolated_url().await;
    let store = store_at(&database).await;
    let mut journal = journal();
    journal.input = serde_json::json!({"secret": "private-input"});
    let id = journal.run.id.0;
    store
        .insert(toolkit_security::AccessScope::allow_all(), journal)
        .await
        .unwrap();
    let queue = Queue::connect(&database, "durable-v1", 1).await.unwrap();
    let mut handler = handler(store, Some(queue));
    handler.probe = Some(Arc::new(BreakAck(
        sea_orm::Database::connect(&database.url).await.unwrap(),
    )));
    let message = message(id, "one");
    let fields = captured(handler.handle(&message)).await;
    assert_fields(&fields, id, "intent_ack", "database", "durable-v1");
    assert_eq!(fields.get("activity_id").map(String::as_str), Some("one"));
}
