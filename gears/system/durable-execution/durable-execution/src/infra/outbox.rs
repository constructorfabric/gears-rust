//! Framework delivery pipeline. Business scheduling remains in the journal.
use crate::config::Config;
use crate::{
    infra::apalis::{Delivery, Queue},
    infra::storage::{JournalStore, StoreError},
};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::BTreeMap;
use std::time::Duration;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxHandle, OutboxMessage, Partitions,
    WorkerTuning,
};

pub const PREFIX: &str = "durable_delivery_outbox";
pub const QUEUE: &str = "activity-delivery-v1";
pub const PAYLOAD_TYPE: &str = "application/json;durable.delivery.v1";
pub async fn start(
    db: toolkit_db::Db,
    handler: impl LeasedMessageHandler + 'static,
) -> anyhow::Result<OutboxHandle> {
    Ok(Outbox::builder(db)
        .table_prefix(PREFIX)?
        .sequencer_tuning(WorkerTuning::sequencer_default().idle_interval(Duration::from_secs(5)))
        .processor_tuning(WorkerTuning::processor_default().idle_interval(Duration::from_secs(5)))
        .reconciler_tuning(WorkerTuning::reconciler().idle_interval(Duration::from_secs(5)))
        .queue(QUEUE, Partitions::of(4))
        .leased(handler)
        .start()
        .await?)
}

pub struct DeliverToApalis {
    pub store: JournalStore,
    pub queues: BTreeMap<String, Queue>,
    pub config: Config,
    #[cfg(all(test, feature = "integration"))]
    pub(super) probe: Option<std::sync::Arc<dyn test_probe::Probe>>,
}
#[async_trait]
impl LeasedMessageHandler for DeliverToApalis {
    async fn handle(&self, message: &OutboxMessage) -> MessageResult {
        if message.payload_type != PAYLOAD_TYPE {
            return MessageResult::Reject("invalid_delivery_type".into());
        }
        let Ok(delivery) = serde_json::from_slice::<Delivery>(&message.payload) else {
            return MessageResult::Reject("invalid_delivery".into());
        };
        let Ok(id) = uuid::Uuid::parse_str(&delivery.run_id) else {
            return MessageResult::Reject("invalid_run_id".into());
        };
        let mut diagnostic = DeliveryDiagnostic::new(id, &delivery);
        let scope = match self.store.worker_scope("dispatch").await {
            Ok(scope) => scope,
            Err(error) => return diagnostic.retry("authorize_read", store_error_category(&error)),
        };
        let journal = match self
            .store
            .get(&scope, durable_execution_sdk::RunId(id))
            .await
        {
            Ok(Some(j)) => j,
            // A scoped miss can mean temporarily revoked tenant access. Keep
            // the shared Outbox command until an authorized read is possible.
            Ok(None) => return diagnostic.retry("journal_read", "run_not_visible"),
            Err(error) => return diagnostic.retry("journal_read", store_error_category(&error)),
        };
        if journal.delivery_generation != delivery.generation
            || journal.run.status.is_terminal()
            || journal.cancellation_requested
            || journal.delivery_activity().as_deref() != Some(&delivery.activity_id)
        {
            return MessageResult::Ok;
        }
        let Some(due) = journal.run.next_attempt_at else {
            return MessageResult::Ok;
        };
        diagnostic.queue = self.config.queue_for(&journal.run.definition);
        let Some(queue) = self.queues.get(diagnostic.queue) else {
            return diagnostic.retry("queue_lookup", "queue_not_configured");
        };
        #[cfg(all(test, feature = "integration"))]
        if self.fail_at(test_probe::Phase::BeforeEnqueue).await {
            return MessageResult::Retry;
        }
        let mut queue = queue.clone();
        if let Err(error) = queue.push_at(delivery, due).await {
            return diagnostic.retry("enqueue", queue_error_category(&error));
        }
        #[cfg(all(test, feature = "integration"))]
        if self.fail_at(test_probe::Phase::AfterEnqueue).await {
            return MessageResult::Retry;
        }
        let scope = match self.store.worker_scope("dispatch").await {
            Ok(scope) => scope,
            Err(error) => return diagnostic.retry("authorize_ack", store_error_category(&error)),
        };
        let intent = uuid::Uuid::new_v5(&id, &journal.delivery_generation.to_be_bytes());
        match self.store.mark_delivered(&scope, intent, Utc::now()).await {
            Ok(()) => {
                #[cfg(all(test, feature = "integration"))]
                if self.fail_at(test_probe::Phase::AfterIntentAck).await {
                    return MessageResult::Retry;
                }
                MessageResult::Ok
            }
            Err(error) => diagnostic.retry("intent_ack", store_error_category(&error)),
        }
    }
}

// Retry has no error payload. Record only allowlisted metadata here; backend
// errors can contain connection URLs, SQL values and credentials.
struct DeliveryDiagnostic<'a> {
    run_id: uuid::Uuid,
    activity_id: String,
    generation: i64,
    queue: &'a str,
}
impl DeliveryDiagnostic<'_> {
    fn new(run_id: uuid::Uuid, delivery: &Delivery) -> Self {
        let activity_id = if delivery.activity_id.is_empty() {
            "<legacy>"
        } else if delivery.activity_id.len() <= 80
            && delivery.activity_id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
            })
        {
            &delivery.activity_id
        } else {
            "<invalid>"
        };
        Self {
            run_id,
            activity_id: activity_id.to_owned(),
            generation: delivery.generation,
            queue: "<unresolved>",
        }
    }

    fn retry(&self, phase: &'static str, category: &'static str) -> MessageResult {
        tracing::warn!(
            run_id = %self.run_id,
            activity_id = %self.activity_id,
            generation = self.generation,
            queue = self.queue,
            phase,
            error_category = category,
            "durable delivery retry"
        );
        MessageResult::Retry
    }
}

fn store_error_category(error: &StoreError) -> &'static str {
    match error {
        StoreError::Database(_) | StoreError::Scope(toolkit_db::secure::ScopeError::Db(_)) => {
            "database"
        }
        StoreError::Scope(_) => "scope",
        StoreError::Codec(_) => "codec",
        StoreError::Conflict => "conflict",
        StoreError::AuthorizationUnavailable | StoreError::Authorization(_) => {
            "authorization_unavailable"
        }
        StoreError::Forbidden => "forbidden",
        StoreError::DefinitionInactive => "definition_inactive",
        StoreError::Invariant(_) => "invariant",
        StoreError::RequestConflict(_) => "request_conflict",
        StoreError::Domain(_) => "domain",
        _ => "storage",
    }
}

fn queue_error_category(error: &anyhow::Error) -> &'static str {
    if error.chain().any(|cause| {
        cause.is::<sqlx::Error>()
            || matches!(
                cause.downcast_ref::<apalis_postgres::Error>(),
                Some(apalis_postgres::Error::Database(_))
            )
    }) {
        "queue_database"
    } else {
        "queue_transport"
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/delivery_diagnostics_tests.rs"]
mod diagnostic_tests;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/outbox_tests.rs"]
mod integration_tests;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/delivery_probe.rs"]
mod test_probe;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
impl DeliverToApalis {
    async fn fail_at(&self, phase: test_probe::Phase) -> bool {
        match &self.probe {
            Some(probe) => probe.fail_at(phase).await,
            None => false,
        }
    }
}
