use crate::{
    observation::{RecordedTime, RunState, RunStatus, StepState, WorkflowQuery},
    prelude::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
#[path = "../../examples/support/mod.rs"]
mod support;

#[tokio::test]
async fn runtime_fanout_claims_multiple_branches_and_joins_in_input_order() {
    let database = crate::store::repository::tests::isolated_url().await;
    temp_env::async_with_vars(
        [("DURABLE_EXAMPLE_PG_URL", Some(database.url.as_str()))],
        Box::pin(async {
            support::run(async |app: &support::App| {
                let started = Arc::new(tokio::sync::Semaphore::new(0));
                let gate = tokio_util::sync::CancellationToken::new();
                let entered = started.clone();
                let release = gate.clone();
                let workflow = WorkflowBuilder::<Vec<u32>>::new("typed.runtime.fanout.v1")
                    .for_each(
                        Step::new(
                            "branch",
                            std::time::Duration::from_secs(90),
                            move |ctx, n: u32| {
                                let entered = entered.clone();
                                let release = release.clone();
                                async move {
                                    assert_eq!(ctx.instance().item_index, Some(n));
                                    entered.add_permits(1);
                                    release.cancelled().await;
                                    Ok(n * 2)
                                }
                            },
                        ),
                        FanOutOptions::default().concurrency(2),
                    )
                    .build()?;
                let reference = workflow.reference();
                app.registry.register_workflow(workflow).await?;
                let admitted = app
                    .client
                    .start(
                        &app.owner,
                        &reference,
                        vec![0, 1, 2],
                        StartOptions::default(),
                    )
                    .await?;
                let permit = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    started.acquire_many(2),
                )
                .await??;
                drop(permit);
                let progress = app.client.get(&app.owner, admitted.run_id()).await?;
                assert_eq!(
                    progress
                        .steps
                        .iter()
                        .filter(|s| matches!(s.state, StepState::Running { .. }))
                        .count(),
                    2
                );
                gate.cancel();
                app.wait(admitted.run_id()).await?;
                assert_eq!(
                    app.client.result(&app.owner, admitted.handle()).await?,
                    vec![0, 2, 4]
                );
                Ok(())
            })
            .await
            .unwrap();
        }),
    )
    .await;
}
#[tokio::test]
async fn typed_parallel_failure_progress_retry_and_cursor_are_consistent() {
    let database = crate::store::repository::tests::isolated_url().await;
    temp_env::async_with_vars(
        [("DURABLE_EXAMPLE_PG_URL", Some(database.url.as_str()))],
        Box::pin(async {
            support::run(async |app: &support::App| {
                let failing = Arc::new(AtomicBool::new(true));
                let failed = failing.clone();
                let sibling_started = Arc::new(tokio::sync::Semaphore::new(0));
                let started = sibling_started.clone();
                let release = tokio_util::sync::CancellationToken::new();
                let gate = release.clone();
                let builder = WorkflowBuilder::<u32>::new("typed.runtime.failure.v1").then(
                    support::step("prepare", |_, n: u32| async move { Ok(n + 1) }),
                );
                let prepared = builder.checkpoint();
                let workflow = builder
                    .parallel((
                        support::step("fallible", move |_, n: u32| {
                            let failed = failed.clone();
                            async move {
                                if failed.load(Ordering::SeqCst) {
                                    Err(ActivityError::permanent("expected_failure"))
                                } else {
                                    Ok(n)
                                }
                            }
                        }),
                        support::step("sibling", move |_, n: u32| {
                            let started = started.clone();
                            let gate = gate.clone();
                            async move {
                                started.add_permits(1);
                                gate.cancelled().await;
                                Ok(n.to_string())
                            }
                        }),
                    ))
                    .then(Step::with_deps(
                        "join",
                        std::time::Duration::from_secs(60),
                        &prepared,
                        |_, (n, text): (u32, String), prepared: u32| async move {
                            Ok(format!("{n}:{text}:{prepared}"))
                        },
                    ))
                    .build()?;
                let reference = workflow.reference();
                app.registry.register_workflow(workflow).await?;
                let accepted = app
                    .client
                    .start(
                        &app.owner,
                        &reference,
                        4,
                        StartOptions::new().idempotency_key(uuid::Uuid::new_v4().to_string()),
                    )
                    .await?;
                let initial = app.client.get(&app.owner, accepted.run_id()).await?;
                sibling_started.acquire().await?.forget();
                let progress = app
                    .wait_status(accepted.run_id(), RunStatus::Failing)
                    .await?;
                assert!(
                    progress
                        .steps
                        .iter()
                        .any(|step| matches!(step.state, StepState::Running { .. }))
                );
                assert!(!progress.state.is_terminal());
                assert!(progress.cursor > initial.cursor);
                let listed = app
                    .inspector
                    .list(
                        &app.owner,
                        WorkflowQuery::new(chrono::Utc::now() - chrono::Duration::hours(1))
                            .status(RunStatus::Failing),
                    )
                    .await?;
                assert!(listed.items.iter().any(|run| run.id == accepted.run_id()));
                assert!(
                    app.client
                        .retry(
                            &app.owner,
                            accepted.run_id(),
                            ContinueOptions::new(progress.epoch)
                        )
                        .await
                        .is_err()
                );
                release.cancel();
                let failure = app.wait(accepted.run_id()).await?;
                assert!(matches!(
                    failure.state,
                    RunState::Failed {
                        completed_at: RecordedTime::Known(_),
                        ..
                    }
                ));
                failing.store(false, Ordering::SeqCst);
                let retried = app
                    .client
                    .retry(
                        &app.owner,
                        accepted.run_id(),
                        ContinueOptions::new(failure.epoch),
                    )
                    .await?;
                assert_eq!(retried.epoch, failure.epoch + 1);
                assert!(matches!(
                    app.client
                        .cancel(
                            &app.owner,
                            accepted.run_id(),
                            CancelOptions::new(failure.epoch)
                        )
                        .await
                        .map_err(|e| e.kind()),
                    Err(crate::ErrorKind::Aborted)
                ));
                let completed = app.wait(accepted.run_id()).await?;
                assert_eq!(
                    app.client
                        .result(&app.owner, &reference.attach(accepted.run_id()))
                        .await?,
                    "5:5:5"
                );
                assert_eq!(completed.steps[0].attempts, 1);
                assert_eq!(completed.steps[2].attempts, 1);
                assert_eq!(completed.steps[1].attempts, 2);
                assert!(matches!(
                    completed.steps[0].state,
                    StepState::Succeeded {
                        checkpoint_epoch: Some(0),
                        ..
                    }
                ));
                let history = app.client.history(&app.owner, accepted.run_id()).await?;
                assert_eq!(history.epochs.len(), 1);
                assert_eq!(history.steps[1].attempts.len(), 2);
                let events = app
                    .client
                    .events(&app.owner, accepted.run_id(), initial.cursor, 100)
                    .await?;
                assert!(
                    events.iter().all(|event| event.sequence > initial.cursor
                        && event.sequence <= completed.cursor)
                );
                let serialized = serde_json::to_string(&completed)?;
                assert!(!serialized.contains("5:5:5"));
                Ok(())
            })
            .await
            .unwrap();
        }),
    )
    .await;
}
#[tokio::test]
async fn typed_checkpoint_survives_runtime_restart_and_late_binding() {
    let database = crate::store::repository::tests::isolated_url().await;
    temp_env::async_with_vars(
        [("DURABLE_EXAMPLE_PG_URL", Some(database.url.as_str()))],
        Box::pin(async {
            let count = Arc::new(AtomicU32::new(0));
            let first_count = count.clone();
            let entered = Arc::new(tokio::sync::Semaphore::new(0));
            let started = entered.clone();
            let workflow = WorkflowBuilder::<u32>::new("typed.runtime.restart.v1")
                .then(support::step("checkpoint", move |_, n: u32| {
                    let count = first_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(n + 1)
                    }
                }))
                .then(support::step("work", move |ctx, _: u32| {
                    let started = started.clone();
                    async move {
                        started.add_permits(1);
                        ctx.cancellation().cancelled().await;
                        Err::<u32, _>(ActivityError::Cancelled)
                    }
                }))
                .build()
                .unwrap();
            let reference = workflow.reference();
            let app = support::App::start().await.unwrap();
            app.registry.register_workflow(workflow).await.unwrap();
            let accepted = app
                .client
                .start(&app.owner, &reference, 10, StartOptions::default())
                .await
                .unwrap();
            entered.acquire().await.unwrap().forget();
            app.shutdown().await.unwrap();
            drop(app);
            let app = support::App::start().await.unwrap();
            let queued = app.client.get(&app.owner, accepted.run_id()).await.unwrap();
            assert_eq!(queued.steps[0].attempts, 1);
            assert!(matches!(queued.state, RunState::Queued));
            let second_count = count.clone();
            let rebound = WorkflowBuilder::<u32>::new("typed.runtime.restart.v1")
                .then(support::step("checkpoint", move |_, n: u32| {
                    let count = second_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(n + 1)
                    }
                }))
                .then(support::step("work", |_, n: u32| async move { Ok(n * 2) }))
                .build()
                .unwrap();
            // This fixture changes a handler's behavior solely to release its test gate; the contract remains identical.
            assert_eq!(
                reference.contract().fingerprint().unwrap(),
                rebound.contract().fingerprint().unwrap()
            );
            app.registry.register_workflow(rebound).await.unwrap();
            app.wait(accepted.run_id()).await.unwrap();
            assert_eq!(
                app.client
                    .result(&app.owner, &reference.attach(accepted.run_id()))
                    .await
                    .unwrap(),
                22
            );
            assert_eq!(count.load(Ordering::SeqCst), 1);
            app.shutdown().await.unwrap();
        }),
    )
    .await;
}
