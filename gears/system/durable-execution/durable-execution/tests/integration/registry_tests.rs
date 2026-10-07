//! Separate database pools share the same catalog and fence admission/claims.
use super::*;
use crate::infra::storage::repository::tests::{isolated_url, store_at};
#[tokio::test]
async fn independent_hosts_order_start_and_unregister_and_reject_delayed_commands() {
    let url = isolated_url().await;
    let first = Arc::new(registrar(store_at(&url).await));
    let second = Arc::new(registrar(store_at(&url).await));
    let d = definition();
    let initial = first.register(d.clone()).await.unwrap();
    assert_eq!(second.register(d.clone()).await.unwrap(), initial);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let task_a = {
        let registrar = first.clone();
        let barrier = barrier.clone();
        let d = d.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            registrar
                .store
                .start(
                    AccessScope::allow_all(),
                    candidate(&d),
                    StartOptions::default(),
                )
                .await
        })
    };
    let task_b = {
        let registrar = second.clone();
        let barrier = barrier.clone();
        let name = d.name.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            registrar
                .unregister(
                    &name,
                    UnregisterOptions {
                        mode: UnregisterMode::CancelAndRelease,
                        expected_revision: 0,
                    },
                )
                .await
        })
    };
    barrier.wait().await;
    let started = task_a.await.unwrap();
    let stopping = task_b.await.unwrap().unwrap();
    assert_eq!(stopping.state, RegistrationState::Stopping);
    assert!(matches!(
        first
            .store
            .start(
                AccessScope::allow_all(),
                candidate(&d),
                StartOptions::default()
            )
            .await,
        Err(crate::domain::error::DomainError::DefinitionInactive)
    ));
    assert!(matches!(
        first.activate(&d.name, initial.revision).await,
        Err(toolkit_canonical_errors::CanonicalError::Aborted { .. })
    ));
    assert!(matches!(
        first.register(d.clone()).await,
        Err(toolkit_canonical_errors::CanonicalError::FailedPrecondition { .. })
    ));
    if let Ok(started) = started {
        executor(&first)
            .execute(started.run_id, 0, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            current(&first.store, started.run_id).await.run.activities[0].attempts,
            0
        );
        executor(&second).reconcile_definitions().await.unwrap();
        assert_eq!(
            current(&first.store, started.run_id).await.run.status,
            RunStatus::Cancelled
        );
    } else {
        assert!(matches!(
            started,
            Err(crate::domain::error::DomainError::DefinitionInactive)
        ));
    }
    executor(&second).reconcile_definitions().await.unwrap();
    executor(&first).reconcile_definitions().await.unwrap();
    let released = first.registration(&d.name).await.unwrap();
    assert_eq!(released.state, RegistrationState::Released);
    assert!(!first.registry.available(&d.name, 0));
    assert!(!second.registry.available(&d.name, 0));
    second.register(d.clone()).await.unwrap();
    let active = first.activate(&d.name, released.revision).await.unwrap();
    assert_eq!(active.generation, 1);
    let start = first
        .store
        .start(
            AccessScope::allow_all(),
            candidate(&d),
            StartOptions::default(),
        )
        .await
        .unwrap();
    executor(&first)
        .execute(start.run_id, 0, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        current(&first.store, start.run_id).await.run.activities[0].attempts,
        0
    );
    executor(&second)
        .execute(start.run_id, 0, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        current(&first.store, start.run_id).await.run.activities[0].status,
        ActivityStatus::Succeeded
    );
}
#[tokio::test]
async fn checkpoint_and_revocation_share_the_catalog_lock_across_pools() {
    let url = isolated_url().await;
    let first = Arc::new(registrar(store_at(&url).await));
    let second = Arc::new(registrar(store_at(&url).await));
    let d = definition();
    first.register(d.clone()).await.unwrap();
    second.register(d.clone()).await.unwrap();
    let mut j = candidate(&d);
    let id = j.run.id;
    first
        .store
        .start(AccessScope::allow_all(), j.clone(), StartOptions::default())
        .await
        .unwrap();
    let claim = j
        .claim(&d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    first
        .store
        .save(AccessScope::allow_all(), 0, j, false)
        .await
        .unwrap();
    let mut checkpoint = current(&first.store, id).await;
    checkpoint
        .complete(claim, serde_json::json!("race"), Utc::now())
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let result_task = {
        let first = first.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            first
                .store
                .save(
                    AccessScope::allow_all(),
                    checkpoint.revision,
                    checkpoint,
                    true,
                )
                .await
        })
    };
    let revoke_task = {
        let second = second.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            stop(&second, UnregisterMode::CancelAndRelease).await
        })
    };
    barrier.wait().await;
    let checkpoint_result = result_task.await.unwrap();
    revoke_task.await.unwrap();
    executor(&second).reconcile_definitions().await.unwrap();
    let j = current(&first.store, id).await;
    if checkpoint_result.is_ok() {
        assert_eq!(j.run.activities[0].result, Some(serde_json::json!("race")));
    } else {
        assert!(matches!(
            checkpoint_result,
            Err(crate::infra::storage::StoreError::DefinitionInactive)
        ));
        assert!(j.run.activities[0].result.is_none());
    }
    let mut late = j.clone();
    if late.owns(claim, Utc::now()) {
        late.complete(claim, serde_json::json!("late"), Utc::now())
            .unwrap();
        assert!(
            first
                .store
                .save(AccessScope::allow_all(), late.revision, late, true)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn hot_binding_redelivers_real_outbox_work_without_replaying_saved_checkpoint() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl ErasedActivity for Count {
        async fn execute(
            &self,
            _: ActivityContext,
            _: ActivityInput,
        ) -> Result<serde_json::Value, ActivityError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!("executed"))
        }
    }
    let store = store().await;
    let registrar = registrar(store.clone());
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let mut d = definition();
    d.activities[0].handler = Arc::new(Count(first.clone()));
    d.activities[1].handler = Arc::new(Count(second.clone()));
    registrar.register_contract(d.contract()).await.unwrap();
    let mut j = candidate(&d);
    let id = j.run.id;
    store
        .start(AccessScope::allow_all(), j.clone(), StartOptions::default())
        .await
        .unwrap();
    // Simulate a checkpoint saved by a previous host, before this host has bindings.
    let claim = j
        .claim(&d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    j.complete(
        claim,
        serde_json::json!("previous-host-checkpoint"),
        Utc::now(),
    )
    .unwrap();
    store
        .save(AccessScope::allow_all(), 0, j, true)
        .await
        .unwrap();
    let queue = format!("late-{}", uuid::Uuid::new_v4());
    let exec = Arc::new(Executor {
        store: store.clone(),
        registry: registrar.registry.clone(),
        config: Config {
            execute_activities: true,
            delivery_enabled: true,
            default_queue: queue.clone(),
            queues: [(queue, 2)].into(),
            service_client_id: "fixture".into(),
            dispatch_interval_secs: 1,
            ..Default::default()
        },
    });
    let database = crate::test_postgres::database().await;
    let runtime = crate::infra::runtime::Runtime::prepare(exec, &database)
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(stop.clone()));
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if store
                .deliveries(&AccessScope::allow_all(), Utc::now(), 100)
                .await
                .unwrap()
                .iter()
                .all(|delivery| delivery.run_id != id || delivery.generation != 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let before = current(&store, id).await;
    assert_eq!(before.run.activities[1].attempts, 0);
    registrar.register(d).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if current(&store, id).await.run.status == RunStatus::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let after = current(&store, id).await;
    assert_eq!(
        after.run.activities[0].result,
        Some(serde_json::json!("previous-host-checkpoint"))
    );
    assert_eq!(after.run.activities[0].attempts, 1);
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn claim_and_revocation_are_ordered_across_independent_hosts() {
    let url = isolated_url().await;
    let first = Arc::new(registrar(store_at(&url).await));
    let second = Arc::new(registrar(store_at(&url).await));
    let d = definition();
    first.register(d.clone()).await.unwrap();
    second.register(d.clone()).await.unwrap();
    let mut claiming = candidate(&d);
    let racing_id = claiming.run.id;
    let late = candidate(&d);
    let late_id = late.run.id;
    for j in [claiming.clone(), late] {
        first
            .store
            .start(AccessScope::allow_all(), j, StartOptions::default())
            .await
            .unwrap();
    }
    claiming
        .claim(&d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let claim_task = {
        let first = first.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            first
                .store
                .save(AccessScope::allow_all(), claiming.revision, claiming, false)
                .await
        })
    };
    let revoke_task = {
        let second = second.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            stop(&second, UnregisterMode::CancelAndRelease).await
        })
    };
    barrier.wait().await;
    let claimed = claim_task.await.unwrap();
    revoke_task.await.unwrap();
    let saved = current(&second.store, racing_id).await;
    match claimed {
        Ok(()) => {
            assert_eq!(saved.run.activities[0].attempts, 1);
            assert!(saved.lease_until.is_some());
        }
        Err(crate::infra::storage::StoreError::DefinitionInactive) => {
            assert_eq!(saved.run.activities[0].attempts, 0);
            assert!(saved.lease_until.is_none());
        }
        other => panic!("unexpected claim result: {other:?}"),
    }
    assert!(saved.run.activities[0].result.is_none());
    let mut late = current(&first.store, late_id).await;
    late.claim(&d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    assert!(matches!(
        first
            .store
            .save(AccessScope::allow_all(), late.revision, late, false)
            .await,
        Err(crate::infra::storage::StoreError::DefinitionInactive)
    ));
    let untouched = current(&second.store, late_id).await;
    assert_eq!(untouched.run.activities[0].attempts, 0);
    assert_eq!(untouched.run.status, RunStatus::Queued);
    assert!(untouched.lease_until.is_none());
    executor(&second).reconcile_definitions().await.unwrap();
    assert_eq!(
        current(&first.store, late_id).await.run.status,
        RunStatus::Cancelled
    );
}
