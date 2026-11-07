use event_broker_sdk::Sequence;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::dlq::{DeadLetterRecord, DeadLetterSink};
use event_broker_sdk::gts_id;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Delivery, EventBrokerError, EventHandler,
    EventV1, Fallback, HandlerOutcome, InMemoryOffsetManager, StoredPosition,
};
use uuid::Uuid;

use crate::consumer::common::{event_type, publish_json, topic, topic_fixture, wait_until};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.dlq.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.dlq.v1~");

#[derive(Clone, Default)]
struct RecordingDeadLetterSink {
    records: Arc<Mutex<Vec<DeadLetterRecord>>>,
    fail: bool,
}

fn rejected_event() -> EventV1 {
    EventV1::builder_for(
        event_type(EVENT_TYPE),
        serde_json::json!({ "reject": true }),
    )
    .id(Uuid::from_u128(0xd1e_0001))
    .tenant_id(Uuid::nil())
    .subject("dlq-1")
    .subject_type_id(event_type(gts_id!("x.eb.showcase.subject.v1~")))
    .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
    .build()
    .unwrap()
    .stamp_position(StoredPosition {
        partition: 0,
        sequence: Sequence::assigned(1),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    })
}

#[async_trait]
impl DeadLetterSink for RecordingDeadLetterSink {
    async fn park(&self, record: DeadLetterRecord) -> Result<(), ConsumerError> {
        if self.fail {
            return Err(EventBrokerError::Internal(
                "dead-letter sink unavailable".to_owned(),
            ));
        }
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

struct HandlerOwnedDlqPolicy {
    sink: RecordingDeadLetterSink,
}

impl HandlerOwnedDlqPolicy {
    async fn park_permanent_failure(
        &self,
        event: &EventV1,
        delivery: &Delivery,
    ) -> Result<(), ConsumerError> {
        let record = DeadLetterRecord::builder(event, delivery, "showcase reject").build();
        self.sink.park(record).await
    }
}

#[async_trait]
impl EventHandler<serde_json::Value> for HandlerOwnedDlqPolicy {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        if event
            .data()
            .and_then(|data| data.get("reject"))
            .and_then(|value| value.as_bool())
            == Some(true)
        {
            self.park_permanent_failure(&event, delivery).await?;
            return Ok(HandlerOutcome::Success);
        }

        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_want_permanent_failures_parked_i_do_it_in_the_handler() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let sink = RecordingDeadLetterSink::default();
    let records = sink.records.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-dlq"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(HandlerOwnedDlqPolicy { sink })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        EVENT_TYPE,
        "dlq-1",
        None,
        serde_json::json!({ "reject": true }),
    )
    .await;

    wait_until(|| records.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    let records = records.lock().unwrap();
    assert_eq!(records[0].reason, "showcase reject");
    assert_eq!(records[0].payload, serde_json::json!({ "reject": true }));
    assert_eq!(records[0].topic, TOPIC);
}

#[tokio::test]
async fn if_my_dead_letter_sink_fails_i_return_an_error_and_the_source_offset_is_not_successful() {
    let sink = RecordingDeadLetterSink {
        fail: true,
        ..RecordingDeadLetterSink::default()
    };
    let records = sink.records.clone();
    let handler = HandlerOwnedDlqPolicy { sink };

    let err = handler
        .park_permanent_failure(&rejected_event(), &Delivery::new(topic(TOPIC), 0, 6))
        .await
        .expect_err("failed parking must keep the handler from returning success");

    assert!(err.to_string().contains("dead-letter sink unavailable"));
    assert!(records.lock().unwrap().is_empty());
}
