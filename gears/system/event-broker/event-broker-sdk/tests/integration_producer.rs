#![cfg(feature = "integration")]
#![allow(unknown_lints, de0901_gts_string_pattern)]
//! Integration tests for `SyncProducer` + `ProducerBuilder` with a mocked backend.
//!
//! Run with: `cargo test -p cyberware-event-broker-sdk --features integration`

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use event_broker_sdk::{
    EventBrokerError, IngestOutcome, ProducerBackend, ProducerBuilder, ProducerCursor, ProducerId,
    ProducerMode, ResetScope, TypedEvent, internal_test_helpers::WireEvent,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
    total_cents: i64,
}

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~test.orders.created.v1";
    const TOPIC: &'static str = "gts.cf.core.events.topic.v1~test.orders.v1";
    const SUBJECT_TYPE: &'static str = "gts.cf.core.events.subject.v1~test.order.v1";
    const SOURCE: &'static str = "order-service";
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.order_id.to_string())
    }
}

struct MockBackend {
    calls: Arc<Mutex<Vec<&'static str>>>,
    outcome: IngestOutcome,
}

#[async_trait]
impl ProducerBackend for MockBackend {
    async fn register_producer(
        &self,
        _: &SecurityContext,
        _: ProducerMode,
        _: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.calls.lock().unwrap().push("register");
        Ok(ProducerId(Uuid::new_v4()))
    }
    async fn ingest_event(
        &self,
        _: &SecurityContext,
        _: &WireEvent,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.calls.lock().unwrap().push("ingest");
        Ok(self.outcome)
    }
    async fn get_producer_cursors(
        &self,
        _: &SecurityContext,
        _: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError> {
        Ok(vec![])
    }
    async fn reset_producer_chain(
        &self,
        _: &SecurityContext,
        _: ProducerId,
        _: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        Ok(())
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn undeclared_type_rejected_without_hitting_backend() {
    let calls = Arc::new(Mutex::new(vec![]));
    let backend = Arc::new(MockBackend {
        calls: calls.clone(),
        outcome: IngestOutcome::Accepted,
    });
    let builder = ProducerBuilder::new_unbound()
        .topics(["gts.cf.core.events.topic.v1~test.unrelated.v1"])
        .event_type_patterns(["gts.cf.core.events.event.v1~test.unrelated.*"])
        .source("test-service")
        .chain_mode(ProducerMode::Stateless);
    // Wire backend manually for test.
    let producer = event_broker_sdk::SyncProducer::build_for_test(builder, backend)
        .await
        .unwrap();
    let event = OrderCreated {
        order_id: Uuid::new_v4(),
        total_cents: 100,
    };
    let err = producer.publish(&ctx(), event).await.unwrap_err();
    assert!(matches!(err, EventBrokerError::EventTypeNotDeclared { .. }));
    // Backend should NOT have been called.
    assert!(calls.lock().unwrap().is_empty());
}
