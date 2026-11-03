//! The log lines of Admission's events and of a dropped plan.

use uuid::Uuid;

use super::admission_events::LogAdmissionEvents;
use super::intake_events::LogIntakeEvents;
use crate::domain::admission::{AdmissionEvents, SensitiveDataEvent};
use crate::domain::plan::Origin;
use crate::domain::record_intake::{DropEvent, DropReason, DropStage, IntakeEvents};
use crate::domain::sensitive_data::SensitiveDataKind;
use crate::test_support::{add_entity, by_a_person, connector_plan};

const SECRET: &str = "a value that must not reach the log";

fn event(kind: SensitiveDataKind) -> SensitiveDataEvent {
    SensitiveDataEvent {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
        origin: Origin::Connector {
            connector: "connector-a".to_owned(),
            provenance: "chat/t/1".to_owned(),
            version: "v1".to_owned(),
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        kind,
    }
}

#[tracing_test::traced_test]
#[test]
fn a_block_is_logged_with_its_kind() {
    let blocked = event(SensitiveDataKind::Health);

    LogAdmissionEvents.blocked(blocked.clone());

    assert!(logs_contain("construct::audit"));
    assert!(logs_contain("event=\"value_blocked\""));
    assert!(logs_contain("kind=\"health\""));
    assert!(logs_contain(&format!("tenant_id={}", blocked.tenant_id)));
    assert!(logs_contain(&format!("subject_id={}", blocked.subject_id)));
    assert!(logs_contain("origin=\"connector\""));
}

#[tracing_test::traced_test]
#[test]
fn a_redaction_is_logged_with_its_kind() {
    LogAdmissionEvents.redacted(event(SensitiveDataKind::Contacts));

    assert!(logs_contain("event=\"value_redacted\""));
    assert!(logs_contain("kind=\"contacts\""));
}

#[tracing_test::traced_test]
#[test]
fn an_admission_drop_is_logged_in_the_record_drop_shape_without_content() {
    let plan = connector_plan(vec![add_entity("note", &[("text", SECRET)])]);
    let event = DropEvent::for_plan(&plan, DropStage::Admission, DropReason::PersonalizationOff)
        .expect("a plan from a record");

    LogIntakeEvents.dropped(event);

    assert!(logs_contain("event=\"record_dropped\""));
    assert!(logs_contain("stage=\"admission\""));
    assert!(logs_contain("reason=\"personalization_off\""));
    assert!(logs_contain("record_key="));
    assert!(
        !logs_contain("chat_engine/thread-42/msg-7"),
        "no provenance"
    );
    assert!(!logs_contain(SECRET), "no content");
}

#[test]
fn a_plan_without_a_record_has_no_record_drop_event() {
    let plan = by_a_person(
        connector_plan(vec![add_entity("note", &[("text", SECRET)])]),
        Origin::Reviewer {
            reviewer: Uuid::new_v4(),
        },
    );

    assert_eq!(
        DropEvent::for_plan(&plan, DropStage::Admission, DropReason::PersonalizationOff),
        None
    );
}
