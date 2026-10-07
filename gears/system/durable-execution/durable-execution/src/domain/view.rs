//! Workflow progress projection for policy-authorized inspection.
//! Execution inputs and checkpoint payloads remain outside this view.
use crate::domain::journal::Journal;
use crate::domain::persisted::{ActivityRun, ActivityStatus, RunStatus};
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/view_contract_tests.rs"]
mod contract_tests;

fn time(
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> durable_execution_sdk::observation::RecordedTime {
    use durable_execution_sdk::observation::RecordedTime;
    at.map_or(RecordedTime::Unknown, RecordedTime::Known)
}
fn public_state(
    run: &crate::domain::persisted::Run,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> durable_execution_sdk::observation::RunState {
    use durable_execution_sdk::observation::RunState;
    let error = run
        .error_code
        .clone()
        .or_else(|| {
            run.activities
                .iter()
                .filter(|a| a.status == ActivityStatus::Failed)
                .find_map(|a| a.error_code.clone())
        })
        .unwrap_or_else(|| "activity_failed".into());
    let failed = run
        .activities
        .iter()
        .any(|a| a.status == ActivityStatus::Failed);
    if failed
        && matches!(
            run.status,
            RunStatus::Running | RunStatus::Queued | RunStatus::RetryWait
        )
    {
        return RunState::Failing { error };
    }
    match run.status {
        RunStatus::Queued => RunState::Queued,
        RunStatus::Running => RunState::Running,
        RunStatus::RetryWait => RunState::RetryWait {
            due_at: run.next_attempt_at,
            error,
        },
        RunStatus::Succeeded => RunState::Succeeded {
            completed_at: time(completed_at),
        },
        RunStatus::Failed
            if run
                .activities
                .iter()
                .any(|a| a.status == ActivityStatus::Running) =>
        {
            RunState::Failing { error }
        }
        RunStatus::Failed => RunState::Failed {
            error,
            completed_at: time(completed_at),
        },
        RunStatus::Cancelling => RunState::Cancelling {
            reason: run.stop_reason.clone(),
        },
        RunStatus::Cancelled => RunState::Cancelled {
            reason: run.stop_reason.clone(),
            completed_at: time(completed_at),
        },
        RunStatus::Blocked => RunState::Blocked { error },
    }
}
fn public_step(
    activity: &ActivityRun,
    epoch: u64,
    stage_number: u32,
) -> durable_execution_sdk::observation::StepProgress {
    use durable_execution_sdk::observation::{StepProgress, StepState};
    let latest = activity
        .attempt_history
        .iter()
        .rev()
        .find(|a| a.epoch == epoch);
    let succeeded = activity
        .attempt_history
        .iter()
        .rev()
        .find(|a| a.status == ActivityStatus::Succeeded);
    let error = activity
        .error_code
        .clone()
        .unwrap_or_else(|| "activity_failed".into());
    let state = match activity.status {
        ActivityStatus::Pending => StepState::Pending,
        ActivityStatus::Running => StepState::Running {
            started_at: time(latest.map(|a| a.started_at).or(activity.started_at)),
        },
        ActivityStatus::RetryWait => StepState::RetryWait { error },
        ActivityStatus::Succeeded => StepState::Succeeded {
            completed_at: time(
                succeeded
                    .and_then(|a| a.finished_at)
                    .or(activity.finished_at),
            ),
            checkpoint_epoch: succeeded.map(|a| a.epoch),
        },
        ActivityStatus::Failed => StepState::Failed {
            error,
            completed_at: time(latest.and_then(|a| a.finished_at).or(activity.finished_at)),
        },
        ActivityStatus::Cancelled => StepState::Cancelled {
            completed_at: time(latest.and_then(|a| a.finished_at)),
        },
    };
    StepProgress {
        id: activity.id.clone(),
        stage: stage_number,
        state,
        attempts: activity.attempts,
    }
}
pub fn progress(
    journal: &Journal,
) -> Result<durable_execution_sdk::observation::RunProgress, crate::domain::error::DomainError> {
    use durable_execution_sdk::observation::RunProgress;
    let run = &journal.run;
    Ok(RunProgress {
        id: run.id,
        definition: run.definition.clone(),
        owner: run.owner.clone(),
        epoch: run.execution_epoch,
        cursor: cursor(journal.revision)?,
        state: public_state(run, journal.completed_at),
        created_at: run.created_at,
        updated_at: run.updated_at,
        steps: run
            .activities
            .iter()
            .enumerate()
            .map(|(i, a)| {
                public_step(
                    a,
                    run.execution_epoch,
                    journal
                        .parallel
                        .as_ref()
                        .and_then(|j| j.steps.get(i))
                        .map_or(u32::try_from(i).unwrap_or(u32::MAX), |s| s.stage),
                )
            })
            .collect(),
    })
}
pub fn history(journal: &Journal) -> durable_execution_sdk::observation::RunHistory {
    use durable_execution_sdk::observation::{
        AttemptOutcome, AttemptState, ExecutionEpoch, RecordedTime, RunHistory, StepAttempt,
        StepHistory,
    };
    let stage_number = |i: usize| {
        journal
            .parallel
            .as_ref()
            .and_then(|j| j.steps.get(i))
            .map_or(u32::try_from(i).unwrap_or(u32::MAX), |s| s.stage)
    };
    RunHistory {
        epochs: journal
            .run
            .previous_executions
            .iter()
            .map(|e| {
                let mut run = journal.run.clone();
                run.status = e.status;
                run.stop_reason.clone_from(&e.stop_reason);
                run.activities.clone_from(&e.activities);
                run.execution_epoch = e.epoch;
                run.error_code = e.activities.iter().find_map(|a| a.error_code.clone());
                ExecutionEpoch {
                    epoch: e.epoch,
                    state: public_state(&run, e.completed_at),
                    steps: e
                        .activities
                        .iter()
                        .enumerate()
                        .map(|(i, a)| public_step(a, e.epoch, stage_number(i)))
                        .collect(),
                }
            })
            .collect(),
        steps: journal
            .run
            .activities
            .iter()
            .map(|activity| StepHistory {
                id: activity.id.clone(),
                attempts: activity
                    .attempt_history
                    .iter()
                    .map(|a| {
                        let outcome = match a.status {
                            ActivityStatus::Succeeded => AttemptOutcome::Succeeded,
                            ActivityStatus::Cancelled => AttemptOutcome::Cancelled,
                            ActivityStatus::Pending | ActivityStatus::Running => {
                                AttemptOutcome::Interrupted {
                                    error: a
                                        .error_code
                                        .clone()
                                        .unwrap_or_else(|| "ownership_lost".into()),
                                }
                            }
                            ActivityStatus::Failed | ActivityStatus::RetryWait => {
                                AttemptOutcome::Failed {
                                    error: a
                                        .error_code
                                        .clone()
                                        .unwrap_or_else(|| "activity_failed".into()),
                                }
                            }
                        };
                        let state = match a.finished_at {
                            Some(finished_at) => AttemptState::Finished {
                                started_at: a.started_at,
                                finished_at,
                                outcome,
                            },
                            None if a.status == ActivityStatus::Running
                                && a.epoch == journal.run.execution_epoch
                                && activity.status == ActivityStatus::Running
                                && activity.attempts == a.number =>
                            {
                                AttemptState::Running {
                                    started_at: a.started_at,
                                }
                            }
                            None => AttemptState::Legacy {
                                started_at: a.started_at,
                                completed_at: RecordedTime::Unknown,
                                outcome,
                            },
                        };
                        StepAttempt {
                            number: a.number,
                            epoch: a.epoch,
                            state,
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/progress_tests.rs"]
mod progress_tests;

/// End of the event sequence range reserved for one committed journal revision.
pub fn cursor(revision: i64) -> Result<u64, crate::domain::error::DomainError> {
    let end = revision
        .checked_mul(256)
        .and_then(|n| n.checked_add(255))
        .ok_or(crate::domain::error::DomainError::Internal(
            "event cursor overflow",
        ))?;
    u64::try_from(end)
        .map_err(|_| crate::domain::error::DomainError::Internal("event cursor overflow"))
}
