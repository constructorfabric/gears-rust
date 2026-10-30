use super::dto::{OutcomeDto, RecordOutcomeDto};
use crate::domain::record_intake::IntakeOutcome;

#[test]
fn each_outcome_has_its_wire_name() {
    for (outcome, dto, wire) in [
        (IntakeOutcome::Received, OutcomeDto::Received, "received"),
        (IntakeOutcome::Repeat, OutcomeDto::Repeat, "repeat"),
    ] {
        let answer = RecordOutcomeDto::from(outcome);
        assert_eq!(answer.outcome, dto);
        assert_eq!(
            serde_json::to_value(&answer).expect("serializes"),
            serde_json::json!({ "outcome": wire })
        );
    }
}
