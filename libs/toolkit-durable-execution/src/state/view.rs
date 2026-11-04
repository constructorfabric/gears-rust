//! Workflow progress projection for policy-authorized inspection.
//! Execution inputs and checkpoint payloads remain outside this view.
use crate::state::journal::Journal;
use crate::state::persisted::{ActivityRun, ActivityStatus, RunStatus};
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/view_contract_tests.rs"]
mod contract_tests;

fn time(at: Option<chrono::DateTime<chrono::Utc>>) -> crate::observation::RecordedTime {
    use crate::observation::RecordedTime;
    at.map_or(RecordedTime::Unknown, RecordedTime::Known)
}
fn public_state(
    run: &crate::state::persisted::Run,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> crate::observation::RunState {
    use crate::observation::RunState;
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
        RunStatus::Compensating => RunState::Compensating {
            cause: crate::observation::CompensationCause::Failure { error },
        },
        RunStatus::CompensationFailed => RunState::CompensationFailed {
            cause: crate::observation::CompensationCause::Failure {
                error: error.clone(),
            },
            error,
        },
        RunStatus::Compensated => RunState::Compensated {
            cause: crate::observation::CompensationCause::Failure { error },
            completed_at: time(completed_at),
        },
        RunStatus::Blocked => RunState::Blocked { error },
    }
}
fn public_step(
    activity: &ActivityRun,
    epoch: u64,
    stage_number: u32,
) -> crate::observation::StepProgress {
    use crate::observation::{StepProgress, StepState};
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
        instance: crate::ActivityInstanceId {
            step: activity.id.clone(),
            item_index: None,
            phase: crate::ExecutionPhase::Forward,
        },
        id: activity.id.clone(),
        stage: stage_number,
        state,
        attempts: activity.attempts,
    }
}
/// Projects the journal into the public progress view of the current epoch.
pub fn progress(journal: &Journal) -> crate::observation::RunProgress {
    use crate::observation::RunProgress;
    let run = &journal.run;
    RunProgress {
        id: run.id,
        definition: run.definition.clone(),
        owner: run.owner.clone(),
        epoch: run.execution_epoch,
        cursor: journal.event_sequence,
        state: state_with_cause(run, journal.completed_at, journal.execution.cause()),
        created_at: run.created_at,
        updated_at: run.updated_at,
        steps: run
            .activities
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let mut projected = public_step(a, run.execution_epoch, stage_number(journal, i));
                if let Some(instance) = journal.execution.instances.get(i) {
                    projected.instance = instance.identity.clone();
                }
                projected
            })
            .collect(),
    }
}
/// Zero-based stage of the activity at `position`; positions past the instances keep their index.
fn stage_number(journal: &Journal, position: usize) -> u32 {
    journal
        .execution
        .instances
        .get(position)
        .map_or(position, |instance| instance.stage)
        .try_into()
        .unwrap_or(u32::MAX)
}
/// Projects the journal into the public history of previous epochs and current step attempts.
pub fn history(journal: &Journal) -> crate::observation::RunHistory {
    use crate::observation::{
        AttemptOutcome, AttemptState, ExecutionEpoch, RecordedTime, RunHistory, StepAttempt,
        StepHistory,
    };
    let stage_number = |i: usize| stage_number(journal, i);
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
                run.error_code.clone_from(&e.error_code);
                ExecutionEpoch {
                    epoch: e.epoch,
                    state: state_with_cause(&run, e.completed_at, e.compensation_cause.as_ref()),
                    steps: e
                        .activities
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            let mut step = public_step(a, e.epoch, stage_number(i));
                            if let Some((identity, stage)) = e.instances.get(i) {
                                step.instance = identity.clone();
                                step.stage = *stage;
                            }
                            step
                        })
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
                            None => AttemptState::Incomplete {
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

fn state_with_cause(
    run: &crate::state::persisted::Run,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    cause: Option<&super::execution::Cause>,
) -> crate::observation::RunState {
    match cause {
        Some(cause) => {
            let cause = match cause {
                super::execution::Cause::Failure(error) => {
                    crate::observation::CompensationCause::Failure {
                        error: error.clone(),
                    }
                }
                super::execution::Cause::Abort(reason) => {
                    crate::observation::CompensationCause::Abort {
                        reason: reason.clone(),
                    }
                }
            };
            match run.status {
                RunStatus::Compensating => crate::observation::RunState::Compensating { cause },
                RunStatus::CompensationFailed => crate::observation::RunState::CompensationFailed {
                    cause,
                    error: run
                        .error_code
                        .clone()
                        .unwrap_or_else(|| "compensation_failed".into()),
                },
                RunStatus::Compensated => crate::observation::RunState::Compensated {
                    cause,
                    completed_at: time(completed_at),
                },
                _ => public_state(run, completed_at),
            }
        }
        None => public_state(run, completed_at),
    }
}
