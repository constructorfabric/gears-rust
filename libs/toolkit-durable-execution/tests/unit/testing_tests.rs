use super::*;
use crate::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

const SECOND: Duration = Duration::from_secs(1);

fn instant_retry(attempts: u32) -> RetryPolicy {
    RetryPolicy::exponential(attempts, SECOND, SECOND)
}

#[tokio::test]
async fn sequential_run_returns_result_checkpoints_and_attempts() {
    let builder = WorkflowBuilder::<u32>::new("testing.sequence.v1").then(Step::new(
        "double",
        SECOND,
        |_, n: u32| async move { Ok(n * 2) },
    ));
    let doubled = builder.checkpoint();
    let workflow = builder
        .then(Step::with_deps(
            "render",
            SECOND,
            &doubled,
            |_, n: u32, d: u32| async move { Ok(format!("{n}/{d}")) },
        ))
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 21).await.unwrap();
    assert_eq!(run.outcome(), &TestOutcome::Succeeded("42/42".to_owned()));
    assert_eq!(run.checkpoint(&doubled), Ok(Some(42)));
    assert_eq!(run.attempts("double"), 1);
    assert_eq!(run.attempts("missing"), 0);
    assert_eq!(run.into_output().unwrap(), "42/42");
}

#[tokio::test]
async fn retryable_failures_consume_the_budget() {
    for (budget, expected) in [(3, Ok(7)), (2, Err("throttled"))] {
        let calls = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&calls);
        let workflow = WorkflowBuilder::<u32>::new("testing.retry.v1")
            .then(
                Step::new("flaky", SECOND, move |ctx, n: u32| {
                    let seen = Arc::clone(&seen);
                    async move {
                        assert_eq!(seen.fetch_add(1, Ordering::SeqCst) + 1, ctx.attempt());
                        if ctx.attempt() < 3 {
                            Err(ActivityError::retryable("throttled"))
                        } else {
                            Ok(n)
                        }
                    }
                })
                .retry(instant_retry(budget)),
            )
            .build()
            .unwrap();
        let run = TestRuntime::new().run(&workflow, 7).await.unwrap();
        assert_eq!(run.attempts("flaky"), budget);
        match expected {
            Ok(value) => assert_eq!(run.into_output().unwrap(), value),
            Err(code) => assert_eq!(
                run.into_output().unwrap_err(),
                TestOutcome::Failed {
                    step: "flaky".into(),
                    code: code.into()
                }
            ),
        }
    }
}

#[tokio::test]
async fn saga_compensates_confirmed_effects_in_reverse_order_with_distinct_keys() {
    let keys = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let forward_keys = Arc::clone(&keys);
    let reverse_keys = Arc::clone(&keys);
    let workflow = WorkflowBuilder::<Vec<u32>>::new("testing.saga.v1")
        .saga()
        .then(
            Step::new("reserve", SECOND, move |ctx, items: Vec<u32>| {
                forward_keys
                    .lock()
                    .unwrap()
                    .push(ctx.idempotency_key().to_owned());
                async move { Ok(items) }
            })
            .compensate(Compensation::new(
                SECOND,
                move |ctx, _: Vec<u32>, _: Vec<u32>| {
                    reverse_keys
                        .lock()
                        .unwrap()
                        .push(ctx.idempotency_key().to_owned());
                    async { Ok(()) }
                },
            )),
        )
        .for_each(
            Step::new("charge", SECOND, |_, n: u32| async move { Ok(n) }).compensate(
                Compensation::new(SECOND, |_, _: u32, _: u32| async { Ok(()) }),
            ),
            FanOutOptions::default(),
        )
        .then(Step::new("activate", SECOND, |_, _: Vec<u32>| async {
            Err::<(), _>(ActivityError::permanent("activation_rejected"))
        }))
        .build()
        .unwrap();
    let run = TestRuntime::new()
        .run(&workflow, vec![1, 2, 3])
        .await
        .unwrap();
    assert_eq!(
        run.outcome(),
        &TestOutcome::Compensated {
            step: "activate".into(),
            code: "activation_rejected".into()
        }
    );
    assert_eq!(
        run.compensated(),
        ["c/charge/2", "c/charge/1", "c/charge/0", "c/reserve/single"]
    );
    let keys = keys.lock().unwrap();
    assert_eq!(
        *keys,
        [
            format!("{}:reserve", run.run_id()),
            format!("{}:c/reserve/single", run.run_id())
        ]
    );
}

#[tokio::test]
async fn exhausted_compensator_stops_reversal() {
    let workflow = WorkflowBuilder::<u32>::new("testing.compensation-failure.v1")
        .saga()
        .then(
            Step::new("pay", SECOND, |_, n: u32| async move { Ok(n) }).compensate(
                Compensation::new(SECOND, |_, _: u32, _: u32| async { Ok(()) }),
            ),
        )
        .then(
            Step::new("reserve", SECOND, |_, n: u32| async move { Ok(n) }).compensate(
                Compensation::new(SECOND, |_, _: u32, _: u32| async {
                    Err(ActivityError::retryable("stock_unavailable"))
                })
                .retry(instant_retry(2)),
            ),
        )
        .then(Step::new("activate", SECOND, |_, _: u32| async {
            Err::<(), _>(ActivityError::permanent("activation_rejected"))
        }))
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 1).await.unwrap();
    assert_eq!(
        run.outcome(),
        &TestOutcome::CompensationFailed {
            step: "activate".into(),
            code: "activation_rejected".into(),
            compensator: "c/reserve/single".into(),
            compensator_code: "stock_unavailable".into()
        }
    );
    assert_eq!(run.attempts("c/reserve/single"), 2);
    assert!(run.compensated().is_empty());
}

#[tokio::test]
async fn failed_branch_without_saga_does_not_stop_siblings() {
    let workflow = WorkflowBuilder::<u32>::new("testing.parallel.v1")
        .parallel((
            Step::new("fails", SECOND, |_, _: u32| async {
                Err::<u32, _>(ActivityError::permanent("rejected"))
            }),
            Step::new("sibling", SECOND, |_, n: u32| async move { Ok(n) }),
        ))
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 1).await.unwrap();
    assert_eq!(run.attempts("sibling"), 1);
    assert!(matches!(run.outcome(), TestOutcome::Failed { step, .. } if step == "fails"));
}

#[tokio::test]
async fn fan_out_limit_and_timeouts_fail_safely() {
    let workflow = WorkflowBuilder::<Vec<u32>>::new("testing.limit.v1")
        .for_each(
            Step::new("item", SECOND, |_, n: u32| async move { Ok(n) }),
            FanOutOptions::default().max_items(2),
        )
        .build()
        .unwrap();
    let run = TestRuntime::new()
        .run(&workflow, vec![1, 2, 3])
        .await
        .unwrap();
    assert!(
        matches!(run.outcome(), TestOutcome::Failed { code, .. } if code == "fanout_limit_exceeded")
    );
    let empty = TestRuntime::new().run(&workflow, vec![]).await.unwrap();
    assert_eq!(empty.into_output().unwrap(), Vec::<u32>::new());

    let workflow = WorkflowBuilder::<u32>::new("testing.timeout.v1")
        .then(
            Step::new("slow", Duration::from_millis(20), |_, n: u32| async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(n)
            })
            .retry(RetryPolicy::no_retry()),
        )
        .build()
        .unwrap();
    let owner = ExecutionOwner {
        tenant_id: Uuid::from_u128(1),
        subject_id: Uuid::from_u128(2),
    };
    let run = TestRuntime::default()
        .owner(owner)
        .run(&workflow, 1)
        .await
        .unwrap();
    assert!(
        matches!(run.outcome(), TestOutcome::Failed { code, .. } if code == "activity_timeout")
    );
}

#[tokio::test]
async fn checkpoint_reads_reject_foreign_and_unbuilt_references() {
    let step = |id: &str| Step::new(id, SECOND, |_, n: u32| async move { Ok(n) });
    let builder = WorkflowBuilder::<u32>::new("testing.identity.v1").then(step("a"));
    let own = builder.checkpoint();
    let workflow = builder.build().unwrap();
    let foreign_builder = WorkflowBuilder::<u32>::new("testing.foreign.v1").then(step("a"));
    let foreign = foreign_builder.checkpoint();
    let _foreign = foreign_builder.build().unwrap();
    let unbuilt = WorkflowBuilder::<u32>::new("testing.identity.v1")
        .then(step("a"))
        .checkpoint();
    let run = TestRuntime::new().run(&workflow, 7).await.unwrap();
    assert_eq!(run.checkpoint(&own), Ok(Some(7)));
    assert_eq!(
        run.checkpoint(&foreign),
        Err(CheckpointReadError::OtherWorkflow)
    );
    assert_eq!(
        run.checkpoint(&unbuilt),
        Err(CheckpointReadError::NotFinalized)
    );
}

#[tokio::test]
async fn forward_instance_limit_matches_the_runtime() {
    let fan_out = |count: usize| {
        WorkflowBuilder::<Vec<u32>>::new("testing.aggregate.v1")
            .for_each(
                Step::new("first", SECOND, |_, n: u32| async move { Ok(n) }),
                FanOutOptions::default(),
            )
            .for_each(
                Step::new("second", SECOND, |_, n: u32| async move { Ok(n) }),
                FanOutOptions::default(),
            )
            .for_each(
                Step::new("third", SECOND, |_, n: u32| async move { Ok(n) }),
                FanOutOptions::default(),
            )
            .for_each(
                Step::new("fourth", SECOND, |_, n: u32| async move { Ok(n) }),
                FanOutOptions::default(),
            )
            .then(Step::new(
                "tail",
                SECOND,
                move |_, items: Vec<u32>| async move { Ok(items.len() + count) },
            ))
            .build()
            .unwrap()
    };
    // Four stages of 1023 items plus one step stay within 4096 instances.
    let within = TestRuntime::new()
        .run(&fan_out(0), (0..1023).collect())
        .await
        .unwrap();
    assert_eq!(within.into_output().unwrap(), 1023);
    // Four stages of 1024 items leave no room for the last step.
    let over = TestRuntime::new()
        .run(&fan_out(0), (0..1024).collect())
        .await
        .unwrap();
    assert_eq!(
        over.outcome(),
        &TestOutcome::Failed {
            step: "tail".into(),
            code: "fanout_limit_exceeded".into()
        }
    );
    assert_eq!(over.attempts("tail"), 0);
}

#[tokio::test]
async fn successful_branches_stay_readable_after_a_sibling_fails() {
    let builder = WorkflowBuilder::<Vec<u32>>::new("testing.partial.v1").for_each(
        Step::new("item", SECOND, |_, n: u32| async move {
            if n == 1 {
                Err(ActivityError::permanent("rejected"))
            } else {
                Ok(n * 10)
            }
        }),
        FanOutOptions::default(),
    );
    let items = builder.checkpoint();
    let (first, second, third) = (items.branch(0), items.branch(1), items.branch(2));
    let run = TestRuntime::new()
        .run(&builder.build().unwrap(), vec![0, 1, 2])
        .await
        .unwrap();
    assert!(matches!(run.outcome(), TestOutcome::Failed { step, .. } if step == "f/item/1"));
    assert_eq!(run.checkpoint(&first), Ok(Some(0)));
    assert_eq!(run.checkpoint(&second), Ok(None));
    assert_eq!(run.checkpoint(&third), Ok(Some(20)));
    assert_eq!(run.checkpoint(&items), Ok(None));
}

#[tokio::test]
async fn panics_and_timeouts_follow_the_executor() {
    let cleaned = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&cleaned);
    let workflow = WorkflowBuilder::<u32>::new("testing.supervision.v1")
        .then(
            Step::new("panics", SECOND, |_, n: u32| async move {
                assert!(n > 100, "handler bug");
                Ok(n)
            })
            .retry(instant_retry(2)),
        )
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 1).await.unwrap();
    assert_eq!(
        run.outcome(),
        &TestOutcome::Failed {
            step: "panics".into(),
            code: "activity_panicked".into()
        }
    );
    assert_eq!(run.attempts("panics"), 2);

    let workflow = WorkflowBuilder::<u32>::new("testing.supervision.v1")
        .then(
            Step::new("slow", Duration::from_millis(20), move |ctx, _: u32| {
                let seen = Arc::clone(&seen);
                async move {
                    ctx.cancellation().cancelled().await;
                    seen.fetch_add(1, Ordering::SeqCst);
                    Err::<u32, _>(ActivityError::Cancelled)
                }
            })
            .retry(RetryPolicy::no_retry()),
        )
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 1).await.unwrap();
    assert!(
        matches!(run.outcome(), TestOutcome::Failed { code, .. } if code == "activity_timeout")
    );
    assert_eq!(cleaned.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn immediate_retries_let_other_tasks_run() {
    let progressed = Arc::new(AtomicU32::new(0));
    let other = Arc::clone(&progressed);
    let background = tokio::spawn(async move {
        loop {
            other.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
        }
    });
    let workflow = WorkflowBuilder::<u32>::new("testing.fairness.v1")
        .then(
            Step::new("flaky", SECOND, |_, _: u32| async {
                Err::<u32, _>(ActivityError::retryable("busy"))
            })
            .retry(instant_retry(50)),
        )
        .build()
        .unwrap();
    let run = TestRuntime::new().run(&workflow, 1).await.unwrap();
    background.abort();
    assert_eq!(run.attempts("flaky"), 50);
    assert!(progressed.load(Ordering::SeqCst) >= 49);
}

#[tokio::test]
async fn debug_output_omits_payloads() {
    let workflow = WorkflowBuilder::<String>::new("testing.debug.v1")
        .then(Step::new(
            "echo",
            SECOND,
            |_, s: String| async move { Ok(s) },
        ))
        .build()
        .unwrap();
    let run = TestRuntime::new()
        .run(&workflow, "private-payload".to_owned())
        .await
        .unwrap();
    let rendered = format!("{run:?} {:?}", run.outcome());
    assert!(!rendered.contains("private-payload"));
    assert!(rendered.contains("Succeeded(..)"));
}

#[tokio::test]
async fn dropping_a_run_cancels_and_aborts_its_handler() {
    use std::sync::atomic::AtomicBool;
    let effect = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (done, signalled) = (Arc::clone(&effect), Arc::clone(&cancelled));
    let workflow = WorkflowBuilder::<u32>::new("testing.drop.v1")
        .then(Step::new("slow", SECOND, move |ctx, n: u32| {
            let (done, signalled) = (Arc::clone(&done), Arc::clone(&signalled));
            async move {
                let token = ctx.cancellation().clone();
                tokio::spawn(async move {
                    token.cancelled().await;
                    signalled.store(true, Ordering::SeqCst);
                });
                tokio::time::sleep(Duration::from_millis(100)).await;
                done.store(true, Ordering::SeqCst);
                Ok(n)
            }
        }))
        .build()
        .unwrap();
    let runtime = TestRuntime::new();
    let outer = tokio::time::timeout(Duration::from_millis(10), runtime.run(&workflow, 1)).await;
    assert!(outer.is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !effect.load(Ordering::SeqCst),
        "the handler outlived its run"
    );
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the handler was not told to stop"
    );
}
