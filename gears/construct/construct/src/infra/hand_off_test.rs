//! The stub hand-off and the drop event's log line.

use serde_json::json;
use uuid::Uuid;

use super::intake_events::LogIntakeEvents;
use super::planner_stub::NoProcessingHandOff;
use crate::domain::error::DomainError;
use crate::domain::record_intake::{
    DropCause, DropEvent, Envelope, IntakeEvents, ReceivedRecord, RecordHandOff,
};

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
        },
        record: json!({ "payload": { "text": SECRET } }),
    }
}

#[test]
fn without_a_planner_the_hand_off_refuses_every_record() {
    let result = NoProcessingHandOff.hand_off(received(Some(Uuid::new_v4())));

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "got {result:?}"
    );
}

#[test]
fn a_drop_event_names_the_record_and_its_cause() {
    let record = received(Some(Uuid::new_v4()));

    let event = DropEvent::for_record(&record, DropCause::ProcessingFailed);

    assert_eq!(event.tenant_id, record.tenant_id);
    assert_eq!(event.connector, record.connector);
    assert_eq!(event.type_id, record.envelope.type_id);
    assert_eq!(event.subject_id, record.envelope.subject_id);
    assert_eq!(event.cause, DropCause::ProcessingFailed);
}

#[tracing_test::traced_test]
#[test]
fn each_planner_cause_is_logged_by_its_name() {
    let record = received(Some(Uuid::new_v4()));

    for cause in [
        DropCause::RoundCap,
        DropCause::TokenCap,
        DropCause::ModelFailed,
    ] {
        LogIntakeEvents.dropped(DropEvent::for_record(&record, cause));
    }

    for name in ["round_cap", "token_cap", "model_failed"] {
        assert!(logs_contain(&format!("cause=\"{name}\"")), "{name}");
    }
}

#[tracing_test::traced_test]
#[test]
fn the_drop_event_is_logged_without_record_content() {
    let subject = Uuid::new_v4();
    let record = received(Some(subject));

    LogIntakeEvents.dropped(DropEvent::for_record(&record, DropCause::ProcessingFailed));

    assert!(logs_contain("construct::audit"));
    assert!(logs_contain("event=\"record_dropped\""));
    assert!(logs_contain(&format!("tenant_id={}", record.tenant_id)));
    assert!(logs_contain(&format!("subject_id={subject}")));
    assert!(logs_contain("cause=\"processing_failed\""));
    assert!(
        !logs_contain("Some("),
        "the subject is logged as a plain id"
    );
    assert!(!logs_contain(SECRET), "no record content");
}
