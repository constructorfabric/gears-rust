//! The stub hand-off and the drop event's log line.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::intake_events::LogIntakeEvents;
use super::planner_stub::DroppingHandOff;
use crate::domain::record_intake::{
    DropEvent, DropReason, DropStage, Envelope, IntakeEvents, ReceivedRecord, RecordHandOff,
};
use crate::test_support::RecordingEvents;

const SECRET: &str = "payload text that must not reach the log";

fn received(subject_id: Option<Uuid>) -> ReceivedRecord {
    ReceivedRecord {
        tenant_id: Uuid::new_v4(),
        connector: Uuid::new_v4().to_string(),
        envelope: Envelope {
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            provenance: "chat_engine/thread-42/msg-7".to_owned(),
            version: "v1".to_owned(),
            subject_id,
            observed_at: time::OffsetDateTime::parse(
                "2026-09-10T08:05:00Z",
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap(),
        },
        record: json!({ "payload": { "text": SECRET } }),
    }
}

#[test]
fn without_a_planner_the_hand_off_drops_the_record_with_an_event() {
    let events = Arc::new(RecordingEvents::default());
    let stub = DroppingHandOff::new(Arc::clone(&events) as Arc<dyn IntakeEvents>);
    let record = received(Some(Uuid::new_v4()));

    let result = stub.hand_off(record.clone());

    assert!(result.is_ok(), "the record is taken: {result:?}");
    let dropped = events.dropped.lock().unwrap().clone();
    assert_eq!(
        dropped,
        vec![DropEvent::for_record(
            &record,
            DropStage::Planner,
            DropReason::NoPlanner
        )]
    );
}

#[test]
fn a_drop_event_names_the_record_its_stage_and_its_reason() {
    let record = received(Some(Uuid::new_v4()));

    let event = DropEvent::for_record(&record, DropStage::Admission, DropReason::ProcessingFailed);

    assert_eq!(event.tenant_id, record.tenant_id);
    assert_eq!(event.subject_id, record.envelope.subject_id);
    assert_eq!(event.connector, record.connector);
    assert_eq!(event.provenance, record.envelope.provenance);
    assert_eq!(event.version, record.envelope.version);
    assert_eq!(event.type_id, record.envelope.type_id);
    assert_eq!(event.stage, DropStage::Admission);
    assert_eq!(event.reason, DropReason::ProcessingFailed);
}

#[tracing_test::traced_test]
#[test]
fn the_drop_event_is_logged_without_record_content() {
    let subject = Uuid::new_v4();
    let record = received(Some(subject));

    LogIntakeEvents.dropped(DropEvent::for_record(
        &record,
        DropStage::Planner,
        DropReason::NoPlanner,
    ));

    assert!(logs_contain("construct::audit"));
    assert!(logs_contain("event=\"record_dropped\""));
    assert!(logs_contain(&format!("tenant_id={}", record.tenant_id)));
    assert!(logs_contain(&format!("subject_id={subject}")));
    assert!(logs_contain("provenance=chat_engine/thread-42/msg-7"));
    assert!(logs_contain("version=v1"));
    assert!(logs_contain("stage=\"planner\""));
    assert!(logs_contain("reason=\"no_planner\""));
    assert!(
        !logs_contain("Some("),
        "the subject is logged as a plain id"
    );
    assert!(!logs_contain(SECRET), "no record content");
}
