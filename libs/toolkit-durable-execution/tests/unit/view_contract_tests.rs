use super::*;
use crate::observation::{AttemptState, RecordedTime, RunState, StepState};
use crate::store::repository::tests::{definition, journal};
use crate::{ActivityError, ActivityId};
use chrono::Utc;
use std::time::Duration;

#[test]
fn archived_parallel_stages_match_the_execution_after_retry() {
    let mut definition = definition().into_parts();
    definition.parallel_groups = vec![
        definition
            .activities
            .iter()
            .map(|step| step.id.clone())
            .collect(),
    ];
    let mut last = definition.activities[0].clone();
    last.id = ActivityId("three".into());
    definition.activities.push(last);
    definition.flow = crate::contracts::DataFlowSpec::independent(
        definition.activities.iter().map(|a| a.id.0.as_str()),
    );
    let definition = definition.build().unwrap();
    let contract = definition.contract();
    let seed = journal();
    let now = Utc::now();
    let mut value = Journal::new(
        seed.run.id,
        seed.run.owner,
        contract,
        serde_json::Value::Null,
        now,
    )
    .unwrap();
    let first = value
        .claim(contract, 0, now, Duration::from_secs(120))
        .unwrap()
        .unwrap();
    let second = value
        .claim(
            contract,
            value.delivery_generation,
            now,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    value
        .complete(first, serde_json::json!("checkpoint"), now)
        .unwrap();
    value
        .fail(second, &ActivityError::permanent("failed"), now, 0)
        .unwrap();
    assert_eq!(value.run.status, RunStatus::Failed);
    let stages: Vec<_> = progress(&value)
        .steps
        .iter()
        .map(|step| step.stage)
        .collect();
    // The failed run never materialized the next stage.
    assert_eq!(stages, [0, 0]);
    value
        .continue_execution(0, RunStatus::Failed, contract, now)
        .unwrap();
    let view = history(&value);
    assert_eq!(
        view.epochs[0]
            .steps
            .iter()
            .map(|step| step.stage)
            .collect::<Vec<_>>(),
        stages
    );
    assert!(matches!(
        view.epochs[0].steps[0].state,
        StepState::Succeeded {
            checkpoint_epoch: Some(0),
            ..
        }
    ));
    assert!(matches!(
        view.epochs[0].steps[1].state,
        StepState::Failed { .. }
    ));
    let retry = value
        .claim(
            contract,
            value.delivery_generation,
            now,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    value.complete(retry, serde_json::Value::Null, now).unwrap();
    let final_step = value
        .claim(
            contract,
            value.delivery_generation,
            now,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    value
        .complete(final_step, serde_json::Value::Null, now)
        .unwrap();
    assert_eq!(value.run.status, RunStatus::Succeeded);
    assert_eq!(
        history(&value).epochs[0]
            .steps
            .iter()
            .map(|step| step.stage)
            .collect::<Vec<_>>(),
        stages
    );
}

#[test]
fn parallel_current_timestamps_follow_one_attempt_in_the_current_epoch() {
    let mut definition = definition().into_parts();
    definition.parallel_groups = vec![
        definition
            .activities
            .iter()
            .map(|step| step.id.clone())
            .collect(),
    ];
    let definition = definition.build().unwrap();
    let contract = definition.contract();
    let seed = journal();
    let now = Utc::now();
    let mut journal = Journal::new(
        seed.run.id,
        seed.run.owner,
        contract,
        serde_json::Value::Null,
        now,
    )
    .unwrap();
    let first = journal
        .claim(contract, 0, now, Duration::from_secs(120))
        .unwrap()
        .unwrap();
    journal
        .fail(first, &ActivityError::retryable("retry"), now, 0)
        .unwrap();
    let sibling = journal
        .claim(
            contract,
            journal.delivery_generation,
            now,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    journal
        .complete(sibling, serde_json::json!("saved"), now)
        .unwrap();
    let retry_at = now + chrono::Duration::seconds(16);
    let retry = journal
        .claim(
            contract,
            journal.delivery_generation,
            retry_at,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    let running = progress(&journal);
    assert_eq!(
        running.steps[first.step].state,
        StepState::Running {
            started_at: RecordedTime::Known(retry_at),
        }
    );
    assert_eq!(
        journal.run.activities[first.step].started_at,
        Some(retry_at)
    );
    assert_eq!(journal.run.activities[first.step].finished_at, None);
    let failed_at = retry_at + chrono::Duration::seconds(1);
    journal
        .fail(retry, &ActivityError::permanent("failed"), failed_at, 0)
        .unwrap();
    let failed = progress(&journal);
    assert!(matches!(failed.steps[first.step].state, StepState::Failed {
        completed_at: RecordedTime::Known(at), ..
    } if at == failed_at));
    let failed_history = history(&journal);
    assert!(matches!(failed_history.steps[first.step].attempts[1].state,
        AttemptState::Finished { started_at, finished_at, .. }
        if started_at == retry_at && finished_at == failed_at));
    journal
        .continue_execution(0, RunStatus::Failed, contract, failed_at)
        .unwrap();
    let pending = progress(&journal);
    assert_eq!(pending.epoch, 1);
    assert_eq!(pending.steps[first.step].state, StepState::Pending);
    assert!(matches!(pending.steps[sibling.step].state,
        StepState::Succeeded { checkpoint_epoch: Some(0), completed_at: RecordedTime::Known(at) }
        if at == now));
    let saved = history(&journal);
    assert_eq!(saved.steps[first.step].attempts.len(), 2);
    assert!(matches!(saved.epochs[0].steps[first.step].state,
        StepState::Failed { completed_at: RecordedTime::Known(at), .. } if at == failed_at));
    let resumed_at = failed_at + chrono::Duration::seconds(1);
    let resumed = journal
        .claim(
            contract,
            journal.delivery_generation,
            resumed_at,
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    let resumed_view = progress(&journal);
    assert_eq!(
        resumed_view.steps[first.step].state,
        StepState::Running {
            started_at: RecordedTime::Known(resumed_at),
        }
    );
    assert!(matches!(
        resumed_view.steps[sibling.step].state,
        StepState::Succeeded {
            checkpoint_epoch: Some(0),
            ..
        }
    ));
    let finished_at = resumed_at + chrono::Duration::seconds(1);
    journal
        .complete(resumed, serde_json::Value::Null, finished_at)
        .unwrap();
    let completed = progress(&journal);
    assert!(matches!(completed.state, RunState::Succeeded { .. }));
    assert!(
        matches!(completed.steps[first.step].state, StepState::Succeeded {
        completed_at: RecordedTime::Known(at), checkpoint_epoch: Some(1),
    } if at == finished_at)
    );
    assert_eq!(history(&journal).steps[first.step].attempts.len(), 3);
}
