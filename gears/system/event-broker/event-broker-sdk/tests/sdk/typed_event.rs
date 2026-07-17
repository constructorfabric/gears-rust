//! Unit tests for TypedEvent and EnvelopedEvent.

use std::borrow::Cow;

use chrono::Utc;
use event_broker_sdk::{EnvelopedEvent, TypedEvent};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OrderCreated {
    order_id: Uuid,
    total_cents: i64,
}

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~example.orders.created.v1";
    const TOPIC: &'static str = "gts.cf.core.events.topic.v1~example.orders.v1";
    const SUBJECT_TYPE: &'static str = "gts.cf.core.events.subject.v1~example.order.v1";
    const SOURCE: &'static str = "order-service";

    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.order_id.to_string())
    }
}

#[test]
fn typed_event_round_trip() {
    let original = OrderCreated {
        order_id: Uuid::new_v4(),
        total_cents: 4299,
    };
    let serialised = serde_json::to_vec(&original).unwrap();
    let restored: OrderCreated = serde_json::from_slice(&serialised).unwrap();
    assert_eq!(original, restored);
}

#[test]
fn enveloped_event_deref() {
    let id = Uuid::new_v4();
    let payload = OrderCreated {
        order_id: id,
        total_cents: 100,
    };
    let now = Utc::now();
    let env = EnvelopedEvent {
        payload: payload.clone(),
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        subject: id.to_string(),
        partition: 3,
        sequence: 42,
        offset: 10,
        occurred_at: now,
        sequence_time: now,
        trace_parent: None,
    };
    assert_eq!(env.order_id, id);
    assert_eq!(env.total_cents, 100);
    assert_eq!(env.sequence, 42);
}
