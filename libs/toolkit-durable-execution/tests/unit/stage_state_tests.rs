use super::*;
use crate::{contracts::InputSource, prelude::*};

fn state(saga: bool, items: usize) -> (ExecutionState, Value, DateTime<Utc>) {
    let builder = WorkflowBuilder::<Vec<u32>>::new("state.expansion.v1");
    let builder = if saga { builder.saga() } else { builder };
    let workflow = builder
        .for_each(
            Step::new("effect", Duration::from_secs(3), |_, n: u32| async move {
                Ok(n)
            })
            .compensate(Compensation::new(
                Duration::from_secs(3),
                |_, _: u32, _: u32| async { Ok(()) },
            )),
            FanOutOptions::default().concurrency(2),
        )
        .build()
        .unwrap();
    let root =
        serde_json::to_value((0..u32::try_from(items).unwrap()).collect::<Vec<_>>()).unwrap();
    let now = Utc::now();
    (
        ExecutionState::new(workflow.reference().contract().clone(), &root, now).unwrap(),
        root,
        now,
    )
}

#[test]
fn pause_resume_abort_and_reverse_order_preserve_confirmed_receipts() {
    let (mut state, root, now) = state(true, 3);
    let fence = state.claim(0, now, Duration::from_secs(30)).unwrap();
    state
        .finish(0, fence, Ok(serde_json::json!(100)), &root, now, 0)
        .unwrap();
    state.pause().unwrap();
    assert!(state.ready(now).is_empty());
    assert_eq!(state.status(now), RunStatus::Cancelled);
    state.continue_run(0, true, &root, now).unwrap();
    assert_eq!(state.instances[0].state.attempts, 1);
    let fence = state.claim(2, now, Duration::from_secs(30)).unwrap();
    state
        .finish(2, fence, Ok(serde_json::json!(102)), &root, now, 0)
        .unwrap();
    let fence = state.claim(1, now, Duration::from_secs(30)).unwrap();
    state
        .abort(Some("operator_abort".into()), &root, now)
        .unwrap();
    assert!(state.ready(now).is_empty());
    assert_eq!(state.instances.len(), 3); // cleanup is not yet confirmed
    state
        .finish(1, fence, Err(ActivityError::Cancelled), &root, now, 0)
        .unwrap();
    assert_eq!(state.status(now), RunStatus::Compensating);
    assert!(state.pause().is_err());
    let reversals = &state.instances[3..];
    assert_eq!(
        reversals
            .iter()
            .map(|i| i.identity.item_index.unwrap())
            .collect::<Vec<_>>(),
        [2, 0]
    );
    assert_eq!(reversals[0].input, serde_json::json!(2));
    assert_eq!(reversals[0].receipt, Some(serde_json::json!(102)));
    for index in [3, 4] {
        assert_eq!(state.ready(now), [index]);
        let fence = state.claim(index, now, Duration::from_secs(30)).unwrap();
        state
            .finish(index, fence, Ok(Value::Null), &root, now, 0)
            .unwrap();
    }
    assert_eq!(state.status(now), RunStatus::Compensated);
    assert!(state.continue_run(1, false, &root, now).is_err());
    assert!(state.continue_run(1, true, &root, now).is_err());
    assert!(!state.abort(None, &root, now).unwrap());
    assert_eq!(
        state.cause(),
        Some(&Cause::Abort(Some("operator_abort".into())))
    );
}

#[test]
fn retries_are_branch_local_and_fences_reject_stale_completion() {
    let (mut state, root, now) = state(false, 3);
    let first = state.claim(0, now, Duration::from_secs(30)).unwrap();
    let second = state.claim(1, now, Duration::from_secs(30)).unwrap();
    assert!(state.ready(now).is_empty()); // the persisted stage cap counts both claims
    state
        .finish(1, second, Ok(serde_json::json!(9)), &root, now, 0)
        .unwrap();
    state
        .finish(
            0,
            first,
            Err(ActivityError::retryable("throttled")),
            &root,
            now,
            0,
        )
        .unwrap();
    let third = state.claim(2, now, Duration::from_secs(30)).unwrap();
    state
        .finish(2, third, Ok(serde_json::json!(8)), &root, now, 0)
        .unwrap();
    let later = now + chrono::Duration::seconds(60);
    let retry = state.claim(0, later, Duration::from_secs(30)).unwrap();
    assert!(
        state
            .finish(0, first, Ok(Value::Null), &root, later, 0)
            .is_err()
    );
    state
        .finish(0, retry, Ok(serde_json::json!(7)), &root, later, 0)
        .unwrap();
    assert_eq!(
        state
            .read(&InputSource::FanOutResults("effect".into()), &root)
            .unwrap(),
        serde_json::json!([7, 9, 8])
    );
    assert_eq!(
        state
            .instances
            .iter()
            .map(|i| i.state.attempts)
            .collect::<Vec<_>>(),
        [2, 1, 1]
    );
    assert!(
        state
            .read(
                &InputSource::FanOutBranch {
                    step: "effect".into(),
                    index: 3
                },
                &root
            )
            .is_err()
    );
}

#[test]
fn expansion_is_durable_and_over_limit_never_invokes_a_branch() {
    let (mut state, root, now) = state(false, 1025);
    assert_eq!(state.status(now), RunStatus::Failed);
    assert!(state.instances.is_empty());
    assert!(state.ready(now).is_empty());
    assert_eq!(state.stage_error.as_deref(), Some("fanout_limit_exceeded"));
    assert!(state.continue_run(0, false, &root, now).is_err());
    let (state, _, _) = self::state(false, 12);
    let restored: ExecutionState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.instances.len(), 12);
    assert_eq!(restored.instances[11].input, serde_json::json!(11));
}

#[test]
fn expired_final_attempt_starts_reversal_without_an_extra_forward_call() {
    let (mut state, root, now) = state(true, 1);
    state.contract.activities[0].retry.max_attempts = 1;
    let fence = state.claim(0, now, Duration::from_secs(1)).unwrap();
    let later = now + chrono::Duration::seconds(2);
    state.recover(&root, later).unwrap();
    assert_eq!(state.status(later), RunStatus::Compensated); // no confirmed effect to reverse
    assert_eq!(state.instances[0].state.attempts, 1);
    assert_eq!(
        state.cause(),
        Some(&Cause::Failure("attempts_exhausted".into()))
    );
    assert!(
        state
            .finish(0, fence, Ok(Value::Null), &root, later, 0)
            .is_err()
    );
}

#[test]
fn completed_reversals_survive_restart_and_manual_retry() {
    let workflow = WorkflowBuilder::<Vec<u32>>::new("state.restart.v1")
        .saga()
        .for_each(
            Step::new("effect", Duration::from_secs(3), |_, n: u32| async move {
                Ok(n)
            })
            .compensate(
                Compensation::new(Duration::from_secs(3), |_, _: u32, _: u32| async { Ok(()) })
                    .retry(RetryPolicy::exponential(
                        1,
                        std::time::Duration::from_secs(15),
                        std::time::Duration::from_mins(5),
                    )),
            ),
            FanOutOptions::default(),
        )
        .then(Step::new(
            "later",
            Duration::from_secs(3),
            |_, _: Vec<u32>| async { Ok(()) },
        ))
        .build()
        .unwrap();
    let root = serde_json::json!([0, 1, 2]);
    let now = Utc::now();
    let mut state = ExecutionState::new(workflow.contract().clone(), &root, now).unwrap();
    for index in 0..3 {
        let fence = state.claim(index, now, Duration::from_secs(30)).unwrap();
        state
            .finish(index, fence, Ok(serde_json::json!(index)), &root, now, 0)
            .unwrap();
    }
    state.abort(None, &root, now).unwrap();
    let first = state.claim(4, now, Duration::from_secs(30)).unwrap();
    state
        .finish(4, first, Ok(Value::Null), &root, now, 0)
        .unwrap();
    let failed = state.claim(5, now, Duration::from_secs(30)).unwrap();
    state
        .finish(
            5,
            failed,
            Err(ActivityError::retryable("provider_down")),
            &root,
            now,
            0,
        )
        .unwrap();
    let mut restored: ExecutionState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    restored.continue_run(0, false, &root, now).unwrap();
    assert_eq!(restored.ready(now), [5]);
    for index in [5, 6] {
        let fence = restored.claim(index, now, Duration::from_secs(30)).unwrap();
        restored
            .finish(index, fence, Ok(Value::Null), &root, now, 0)
            .unwrap();
    }
    assert_eq!(restored.instances[4].state.attempts, 1);
    assert_eq!(restored.instances[5].state.attempts, 2);
    assert_eq!(restored.status(now), RunStatus::Compensated);
}

#[test]
fn run_cap_includes_sequential_steps_after_fanout_and_never_creates_over_limit_instances() {
    let mut builder = WorkflowBuilder::<Vec<u32>>::new("state.run-limit.v1");
    for index in 0..4 {
        builder = builder
            .for_each(
                Step::new(
                    format!("stage-{index}"),
                    Duration::from_secs(3),
                    |_, n: u32| async move { Ok(n) },
                ),
                FanOutOptions::default(),
            )
            .then(Step::new(
                format!("pass-{index}"),
                Duration::from_secs(3),
                |_, v: Vec<u32>| async move { Ok(v) },
            ));
    }
    let contract = builder
        .then(Step::new(
            "over-limit",
            Duration::from_secs(3),
            |_, v: Vec<u32>| async move { Ok(v.len()) },
        ))
        .build()
        .unwrap()
        .contract()
        .clone();
    let root = serde_json::to_value((0..1023u32).collect::<Vec<_>>()).unwrap();
    let now = Utc::now();
    let mut engine = ExecutionState::new(contract, &root, now).unwrap();
    while let Some(index) = engine.ready(now).first().copied() {
        let value = engine.instances[index].input.clone();
        let fence = engine.claim(index, now, Duration::from_secs(30)).unwrap();
        engine
            .finish(index, fence, Ok(value), &root, now, 0)
            .unwrap();
    }
    assert_eq!(engine.status(now), RunStatus::Failed);
    assert_eq!(engine.instances.len(), 4096);
    assert_eq!(engine.stage_error.as_deref(), Some("fanout_limit_exceeded"));
}
