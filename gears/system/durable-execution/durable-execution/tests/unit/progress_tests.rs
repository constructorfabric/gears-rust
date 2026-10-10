use super::*;
use chrono::Utc;
use durable_execution_sdk::observation::{
    AttemptOutcome, AttemptState, RecordedTime, RunState, StepState,
};
fn fixture() -> Journal {
    crate::infra::storage::repository::tests::journal()
}
#[test]
fn partial_failure_is_failing_until_independent_work_settles() {
    let mut journal = fixture();
    journal.run.status = RunStatus::Running;
    journal.run.activities[0].status = ActivityStatus::Failed;
    journal.run.activities[0].error_code = Some("worker_failed".into());
    journal.run.activities[1].status = ActivityStatus::Running;
    journal.run.error_code = Some("worker_failed".into());
    assert!(matches!(
        progress(&journal).unwrap().state,
        RunState::Failing { .. }
    ));
    journal.run.status = RunStatus::Failed;
    journal.run.activities[1].status = ActivityStatus::Succeeded;
    assert!(matches!(
        progress(&journal).unwrap().state,
        RunState::Failed {
            completed_at: RecordedTime::Unknown,
            ..
        }
    ));
}
#[test]
fn progress_omits_results_history_and_fences_but_retains_checkpoint_origin() {
    let mut journal = fixture();
    let at = Utc::now();
    journal.run.execution_epoch = 2;
    let activity = &mut journal.run.activities[0];
    activity.status = ActivityStatus::Succeeded;
    activity.result = Some(serde_json::json!({"secret":"never-in-progress"}));
    activity.attempts = 1;
    activity
        .attempt_history
        .push(crate::domain::persisted::ActivityAttempt {
            number: 1,
            epoch: 0,
            fence: 19,
            started_at: at,
            finished_at: Some(at),
            status: ActivityStatus::Succeeded,
            error_code: None,
        });
    let progress = progress(&journal).unwrap();
    assert!(matches!(
        progress.steps[0].state,
        StepState::Succeeded {
            checkpoint_epoch: Some(0),
            ..
        }
    ));
    let json = serde_json::to_string(&progress).unwrap();
    for secret in [
        "never-in-progress",
        "fence",
        "budget_attempts",
        "previous_executions",
        "attempt_history",
    ] {
        assert!(!json.contains(secret));
    }
}
#[test]
fn interrupted_attempt_is_not_success_or_business_cancellation() {
    let mut journal = fixture();
    let at = Utc::now();
    let activity = &mut journal.run.activities[0];
    activity.status = ActivityStatus::Pending;
    activity.attempts = 1;
    activity
        .attempt_history
        .push(crate::domain::persisted::ActivityAttempt {
            number: 1,
            epoch: 0,
            fence: 7,
            started_at: at,
            finished_at: Some(at),
            status: ActivityStatus::Pending,
            error_code: Some("lease_expired".into()),
        });
    let history = history(&journal);
    assert!(
        matches!(&history.steps[0].attempts[0].state,AttemptState::Finished{outcome:AttemptOutcome::Interrupted{error},..} if error=="lease_expired")
    );
}
#[test]
fn new_completions_have_confirmed_time_legacy_records_remain_unknown() {
    let mut journal = fixture();
    journal.run.status = RunStatus::Succeeded;
    assert!(matches!(
        progress(&journal).unwrap().state,
        RunState::Succeeded {
            completed_at: RecordedTime::Unknown
        }
    ));
    let at = Utc::now();
    journal.completed_at = Some(at);
    assert!(
        matches!(progress(&journal).unwrap().state,RunState::Succeeded{completed_at:RecordedTime::Known(time)} if time==at)
    );
    let mut wire = serde_json::to_value(&journal).unwrap();
    wire.as_object_mut().unwrap().remove("completed_at");
    let legacy = serde_json::from_value::<Journal>(wire).unwrap();
    assert!(legacy.completed_at.is_none());
}

#[test]
fn legacy_checkpoint_without_attempt_metadata_has_unknown_origin() {
    let mut journal = fixture();
    journal.run.execution_epoch = 3;
    journal.run.activities[0].status = ActivityStatus::Succeeded;
    journal.run.activities[0].result = Some(serde_json::Value::Null);
    assert!(matches!(
        progress(&journal).unwrap().steps[0].state,
        StepState::Succeeded {
            checkpoint_epoch: None,
            completed_at: RecordedTime::Unknown
        }
    ));
}
