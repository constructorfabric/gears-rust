//! Real executor/storage regressions for runtime expansion and compensation.
use super::*;
use crate::contracts::ExecutionDefinition;
use crate::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use crate::state::persisted::RunStatus;
use crate::store::repository::tests::store;
use crate::{ExecutionOwner, prelude::*};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use toolkit_security::AccessScope;

async fn fixture(definition: ExecutionDefinition, input: serde_json::Value) -> (Executor, RunId) {
    let store = store().await;
    let registry = Arc::new(Registry::default());
    let journal = Journal::new(
        RunId(uuid::Uuid::new_v4()),
        ExecutionOwner {
            tenant_id: uuid::Uuid::new_v4(),
            subject_id: uuid::Uuid::new_v4(),
        },
        definition.contract(),
        input,
        Utc::now(),
    )
    .unwrap();
    let id = journal.run.id;
    store
        .insert(AccessScope::allow_all(), journal)
        .await
        .unwrap();
    registry.register(definition).unwrap();
    (
        Executor {
            store,
            registry,
            config: Config::default(),
        },
        id,
    )
}
async fn drive(executor: &Executor, id: RunId) -> Journal {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let journal = executor
                .store
                .get(&AccessScope::allow_all(), id)
                .await
                .unwrap()
                .unwrap();
            if journal.run.status.is_terminal()
                || journal.run.status == RunStatus::CompensationFailed
            {
                return journal;
            }
            if journal.run.next_attempt_at.is_some_and(|t| t > Utc::now()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            executor
                .execute(id, journal.delivery_generation, CancellationToken::new())
                .await
                .unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn explicit_delivery_claims_a_nonfirst_branch_and_duplicate_is_fenced() {
    let workflow = WorkflowBuilder::<Vec<u32>>::new("staged.target.v1")
        .for_each(
            Step::new("effect", Duration::from_secs(3), |_, n: u32| async move {
                Ok(n * 2)
            }),
            FanOutOptions::default(),
        )
        .build()
        .unwrap();
    let (executor, id) = fixture(workflow.into_definition(), serde_json::json!([7, 3, 9])).await;
    let budget = super::super::shutdown::ShutdownBudget::new(Duration::from_secs(3));
    executor
        .execute_target(id, 0, Some("f/effect/2"), CancellationToken::new(), &budget)
        .await
        .unwrap();
    let journal = executor
        .store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        journal.run.activities[2].result,
        Some(serde_json::json!(18))
    );
    assert_eq!(journal.run.activities[0].attempts, 0);
    assert_eq!(journal.run.activities[1].attempts, 0);
    executor
        .execute_target(id, 0, Some("f/effect/2"), CancellationToken::new(), &budget)
        .await
        .unwrap();
    let journal = executor
        .store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(journal.run.activities[2].attempts, 1);
}

#[tokio::test]
async fn competing_claims_use_cas_and_observe_persisted_stage_concurrency() {
    let workflow = WorkflowBuilder::<Vec<u32>>::new("staged.race.v1")
        .for_each(
            Step::new("effect", Duration::from_secs(3), |_, n: u32| async move {
                Ok(n)
            }),
            FanOutOptions::default().concurrency(2),
        )
        .build()
        .unwrap();
    let contract = workflow.contract().clone();
    let (executor, id) = fixture(workflow.into_definition(), serde_json::json!([7, 3, 9])).await;
    let scope = AccessScope::allow_all();
    let mut first = executor.store.get(&scope, id).await.unwrap().unwrap();
    let mut second = first.clone();
    let revision = first.revision;
    first
        .claim_target(
            &contract,
            0,
            Some("f/effect/0"),
            Utc::now(),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    second
        .claim_target(
            &contract,
            0,
            Some("f/effect/1"),
            Utc::now(),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (a, b) = tokio::join!(
        async {
            barrier.wait().await;
            executor
                .store
                .save(scope.clone(), revision, first, true)
                .await
        },
        async {
            barrier.wait().await;
            executor
                .store
                .save(scope.clone(), revision, second, true)
                .await
        }
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let mut persisted = executor.store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(
        persisted
            .run
            .activities
            .iter()
            .filter(|a| a.status == crate::state::persisted::ActivityStatus::Running)
            .count(),
        1
    );
    let revision = persisted.revision;
    let target = persisted
        .execution
        .instances
        .iter()
        .find(|i| i.state.status == crate::state::persisted::ActivityStatus::Pending)
        .unwrap()
        .identity
        .key();
    persisted
        .claim_target(
            &contract,
            persisted.delivery_generation,
            Some(&target),
            Utc::now(),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    executor
        .store
        .save(scope.clone(), revision, persisted, true)
        .await
        .unwrap();
    let mut persisted = executor.store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(
        persisted
            .run
            .activities
            .iter()
            .filter(|a| a.status == crate::state::persisted::ActivityStatus::Running)
            .count(),
        2
    );
    assert!(persisted.execution.ready(Utc::now()).is_empty());
    assert!(
        persisted
            .claim(
                &contract,
                persisted.delivery_generation,
                Utc::now(),
                Duration::from_secs(30)
            )
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn fanout_executes_per_run_lists_and_keeps_order_across_recreated_executor() {
    for items in [vec![], vec![7u32], vec![7, 3, 9]] {
        let workflow = WorkflowBuilder::<Vec<u32>>::new("staged.fanout.v1")
            .for_each(
                Step::new("double", Duration::from_secs(5), |_, n: u32| async move {
                    Ok(n * 2)
                }),
                FanOutOptions::default(),
            )
            .then(Step::new(
                "join",
                Duration::from_secs(5),
                |_, items: Vec<u32>| async move { Ok(items) },
            ))
            .build()
            .unwrap();
        let (executor, id) = fixture(
            workflow.into_definition(),
            serde_json::to_value(&items).unwrap(),
        )
        .await;
        let journal = drive(&executor, id).await;
        assert_eq!(journal.run.status, RunStatus::Succeeded);
        let expected =
            serde_json::to_value(items.iter().map(|n| n * 2).collect::<Vec<_>>()).unwrap();
        assert_eq!(journal.execution.checkpoints()["join"], expected);
        let restored = Executor {
            store: executor.store.clone(),
            registry: executor.registry.clone(),
            config: executor.config.clone(),
        };
        restored
            .execute(id, journal.delivery_generation, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            restored
                .store
                .get(&AccessScope::allow_all(), id)
                .await
                .unwrap()
                .unwrap()
                .execution
                .instances
                .len(),
            items.len() + 1
        );
    }
}
#[tokio::test]
async fn compensation_failure_retry_preserves_forward_receipt_and_does_not_repeat_successful_cleanup()
 {
    let blocked = Arc::new(AtomicBool::new(true));
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let one = first.clone();
    let two = second.clone();
    let gate = blocked.clone();
    let workflow = WorkflowBuilder::<u32>::new("staged.saga.v1")
        .saga()
        .then(
            Step::new("first", Duration::from_secs(5), |_, n: u32| async move {
                Ok(n + 1)
            })
            .compensate(Compensation::new(
                Duration::from_secs(5),
                move |ctx, input: u32, receipt: u32| {
                    let one = one.clone();
                    async move {
                        assert!(ctx.idempotency_key().contains("c/first"));
                        assert_eq!(receipt, input + 1);
                        one.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )),
        )
        .then(
            Step::new("second", Duration::from_secs(5), |_, n: u32| async move {
                Ok(n + 1)
            })
            .compensate(Compensation::new(
                Duration::from_secs(5),
                move |_, input: u32, receipt: u32| {
                    let two = two.clone();
                    let gate = gate.clone();
                    async move {
                        assert_eq!(receipt, input + 1);
                        two.fetch_add(1, Ordering::SeqCst);
                        if gate.load(Ordering::SeqCst) {
                            Err(ActivityError::permanent("reverse_blocked"))
                        } else {
                            Ok(())
                        }
                    }
                },
            )),
        )
        .then(Step::new(
            "fail",
            Duration::from_secs(5),
            |_, _: u32| async move { Err::<u32, _>(ActivityError::permanent("business_failure")) },
        ))
        .build()
        .unwrap();
    let definition = workflow.into_definition();
    let (executor, id) = fixture(definition.clone(), serde_json::json!(3)).await;
    let mut stopped = drive(&executor, id).await;
    assert_eq!(stopped.run.status, RunStatus::CompensationFailed);
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    assert!(stopped.request_cancel_at(0, Utc::now(), None).is_err());
    let stopping = executor
        .store
        .unregister(
            "staged.saga.v1",
            UnregisterOptions::new(UnregisterMode::CancelAndRelease, 0),
        )
        .await
        .unwrap();
    executor
        .store
        .reconcile_definition(&stopping)
        .await
        .unwrap();
    assert_eq!(
        executor
            .store
            .definition("staged.saga.v1")
            .await
            .unwrap()
            .state,
        RegistrationState::Stopping
    );
    let registrar = crate::catalog::registrar::Registrar {
        store: executor.store.clone(),
        registry: Arc::new(Registry::default()),
    };
    assert_eq!(
        registrar.register(definition).await.unwrap().state,
        RegistrationState::Stopping
    );
    let executor = Executor {
        store: executor.store.clone(),
        registry: registrar.registry,
        config: executor.config.clone(),
    };
    blocked.store(false, Ordering::SeqCst);
    let contract = stopped.execution.contract.clone();
    stopped
        .continue_execution(0, RunStatus::Failed, &contract, Utc::now())
        .unwrap();
    executor
        .store
        .save(AccessScope::allow_all(), stopped.revision, stopped, true)
        .await
        .unwrap();
    let completed = drive(&executor, id).await;
    assert_eq!(completed.run.status, RunStatus::Compensated);
    executor
        .store
        .reconcile_definition(&stopping)
        .await
        .unwrap();
    assert_eq!(
        executor
            .store
            .definition("staged.saga.v1")
            .await
            .unwrap()
            .state,
        RegistrationState::Released
    );
    let history = crate::state::view::history(&completed);
    assert!(
        matches!(&history.epochs[0].state, crate::observation::RunState::CompensationFailed { cause: crate::observation::CompensationCause::Failure { error }, .. } if error=="business_failure")
    );
    assert!(
        history.epochs[0]
            .steps
            .iter()
            .any(|step| step.instance.phase == crate::ExecutionPhase::Compensation)
    );
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 2);
    assert!(
        matches!(completed.execution.cause(),Some(crate::state::execution::Cause::Failure(code)) if code=="business_failure")
    );
    assert_eq!(
        completed.execution.checkpoints()["first"],
        serde_json::json!(4)
    );
}
#[tokio::test]
async fn large_expansion_commits_more_than_256_events_and_payload_free_progress() {
    let workflow = WorkflowBuilder::<Vec<u32>>::new("staged.large.v1")
        .for_each(
            Step::new(
                "item",
                Duration::from_secs(5),
                |_, n: u32| async move { Ok(n) },
            ),
            FanOutOptions::default(),
        )
        .build()
        .unwrap();
    let (executor, id) = fixture(
        workflow.into_definition(),
        serde_json::to_value(vec![1u32; 260]).unwrap(),
    )
    .await;
    let (sender, mut deliveries) = tokio::sync::mpsc::unbounded_channel();
    let consumer = executor
        .store
        .start_outbox(crate::store::repository::tests::CaptureDelivery(sender))
        .await
        .unwrap();
    let indexes = tokio::time::timeout(Duration::from_secs(45), async {
        let mut indexes = std::collections::BTreeSet::new();
        for _ in 0..260 {
            let delivery = deliveries.recv().await.unwrap();
            assert_eq!(delivery.run_id, id.0.to_string());
            assert_eq!(delivery.generation, 0);
            assert_eq!(delivery.activity_id, delivery.instance.key());
            assert!(indexes.insert(delivery.instance.item_index.unwrap()));
        }
        indexes
    })
    .await
    .unwrap();
    assert_eq!(indexes.len(), 260);
    consumer.stop().await;
    let progress = executor
        .store
        .get_progress(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.steps.len(), 260);
    assert!(progress.cursor > 256);
    let serialized = serde_json::to_string(&progress).unwrap();
    assert!(!serialized.contains("\"input\""));
    assert!(!serialized.contains("\"result\""));
    let journal = executor
        .store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.cursor, journal.event_sequence);
    let mut engine = journal.execution;
    let now = Utc::now();
    for _ in 0..16 {
        let index = engine.ready(now)[0];
        engine.claim(index, now, Duration::from_secs(30)).unwrap();
    }
    assert!(engine.ready(now).is_empty());
}
