use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_arms_budget_while_both_components_are_stalled() {
    let stop = CancellationToken::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let worker_dropped = Arc::new(AtomicBool::new(false));
    let dispatcher_dropped = Arc::new(AtomicBool::new(false));
    let outbox_stopped = Arc::new(AtomicBool::new(false));
    let worker = {
        let barrier = barrier.clone();
        let dropped = worker_dropped.clone();
        async move {
            let _guard = Dropped(dropped);
            barrier.wait().await;
            std::future::pending::<anyhow::Result<()>>().await
        }
    };
    let dispatcher = {
        let barrier = barrier.clone();
        let dropped = dispatcher_dropped.clone();
        async move {
            let _guard = Dropped(dropped);
            barrier.wait().await;
            std::future::pending::<anyhow::Result<()>>().await
        }
    };
    let outbox = {
        let stopped = outbox_stopped.clone();
        async move {
            stopped.store(true, Ordering::SeqCst);
        }
    };
    let task_stop = stop.clone();
    let task = tokio::spawn(async move {
        run_components(
            worker,
            dispatcher,
            task_stop,
            &super::super::shutdown::ShutdownBudget::new(Duration::from_secs(3)),
            outbox,
        )
        .await
    });
    barrier.wait().await;
    let began = tokio::time::Instant::now();
    stop.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("shutdown timed out"));
    assert_eq!(began.elapsed(), Duration::from_millis(2500));
    assert!(worker_dropped.load(Ordering::SeqCst));
    assert!(dispatcher_dropped.load(Ordering::SeqCst));
    assert!(outbox_stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn component_failure_cancels_and_drains_peers_before_returning_error() {
    let stop = CancellationToken::new();
    let peer = stop.clone();
    let drained = Arc::new(AtomicBool::new(false));
    let observed = drained.clone();
    let result = run_components(
        async { Err(anyhow::anyhow!("consumer failed")) },
        async move {
            peer.cancelled().await;
            observed.store(true, Ordering::SeqCst);
            Ok(())
        },
        stop.clone(),
        &super::super::shutdown::ShutdownBudget::new(Duration::from_secs(1)),
        async {},
    )
    .await;
    assert_eq!(result.unwrap_err().to_string(), "consumer failed");
    assert!(stop.is_cancelled());
    assert!(drained.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cooperative_components_finish_cleanly_after_cancellation() {
    let stop = CancellationToken::new();
    let worker = stop.clone();
    let dispatcher = stop.clone();
    stop.cancel();
    run_components(
        async move {
            worker.cancelled().await;
            Ok(())
        },
        async move {
            dispatcher.cancelled().await;
            Ok(())
        },
        stop,
        &super::super::shutdown::ShutdownBudget::new(Duration::from_secs(1)),
        async {},
    )
    .await
    .unwrap();
}

#[derive(Clone, Copy)]
enum DispatchOutcome {
    Database,
    ScopedDatabase,
    Success,
    Permanent,
    DeniedScope,
}

struct DispatchAuthorization {
    script: Vec<DispatchOutcome>,
    calls: std::sync::atomic::AtomicUsize,
    observed: tokio::sync::mpsc::UnboundedSender<usize>,
}

#[async_trait]
impl crate::infra::authorization::WorkerAuthorization for DispatchAuthorization {
    async fn scope(
        &self,
        action: &str,
    ) -> Result<toolkit_security::AccessScope, crate::infra::storage::StoreError> {
        use crate::infra::storage::StoreError;
        if action != "definition:list" {
            return Ok(toolkit_security::AccessScope::allow_all());
        }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed.send(call).unwrap();
        match self
            .script
            .get(call)
            .copied()
            .unwrap_or(DispatchOutcome::Permanent)
        {
            DispatchOutcome::Database => Err(StoreError::Database(toolkit_db::DbError::Sea(
                sea_orm::DbErr::Custom("fixture database unavailable".into()),
            ))),
            DispatchOutcome::ScopedDatabase => {
                Err(StoreError::Scope(toolkit_db::secure::ScopeError::Db(
                    sea_orm::DbErr::Custom("fixture scoped database unavailable".into()),
                )))
            }
            DispatchOutcome::Success => Ok(toolkit_security::AccessScope::allow_all()),
            DispatchOutcome::Permanent => Err(StoreError::DeliveryUnavailable),
            DispatchOutcome::DeniedScope => Err(StoreError::Scope(
                toolkit_db::secure::ScopeError::Denied("fixture scope denied"),
            )),
        }
    }
}

async fn dispatch_runtime(
    script: Vec<DispatchOutcome>,
    interval: u64,
) -> (Runtime, tokio::sync::mpsc::UnboundedReceiver<usize>) {
    let (observed, calls) = tokio::sync::mpsc::unbounded_channel();
    let store = crate::infra::storage::repository::tests::store()
        .await
        .with_authorization(Arc::new(DispatchAuthorization {
            script,
            calls: std::sync::atomic::AtomicUsize::new(0),
            observed,
        }));
    (
        Runtime {
            executor: Arc::new(Executor {
                store,
                registry: Arc::new(crate::domain::registry::Registry::default()),
                config: crate::config::Config {
                    dispatch_interval_secs: interval,
                    control_plane_failure_limit: 2,
                    ..crate::config::Config::default()
                },
            }),
            queues: BTreeMap::new(),
            outbox: None,
        },
        calls,
    )
}

#[tokio::test]
async fn dispatcher_retries_both_database_wrappers_and_resets_only_after_success() {
    let (runtime, mut calls) = dispatch_runtime(
        vec![
            DispatchOutcome::Database,
            DispatchOutcome::Success,
            DispatchOutcome::ScopedDatabase,
            DispatchOutcome::Success,
            DispatchOutcome::Permanent,
        ],
        1,
    )
    .await;
    let task = tokio::spawn(async move { runtime.dispatch(CancellationToken::new()).await });
    for expected in 0..5 {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), calls.recv())
                .await
                .unwrap(),
            Some(expected)
        );
    }
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::infra::storage::StoreError>(),
        Some(crate::infra::storage::StoreError::DeliveryUnavailable)
    ));
    assert_eq!(calls.recv().await, None);
}

#[tokio::test]
async fn dispatcher_stops_at_database_limit_and_does_not_retry_scope_denial() {
    for script in [
        vec![DispatchOutcome::Database, DispatchOutcome::ScopedDatabase],
        vec![DispatchOutcome::DeniedScope],
    ] {
        let count = script.len();
        let (runtime, mut calls) = dispatch_runtime(script, 1).await;
        let task = tokio::spawn(async move { runtime.dispatch(CancellationToken::new()).await });
        for expected in 0..count {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), calls.recv())
                    .await
                    .unwrap(),
                Some(expected)
            );
        }
        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        match (
            count,
            error.downcast_ref::<crate::infra::storage::StoreError>(),
        ) {
            (
                2,
                Some(crate::infra::storage::StoreError::Scope(toolkit_db::secure::ScopeError::Db(
                    _,
                ))),
            )
            | (
                1,
                Some(crate::infra::storage::StoreError::Scope(
                    toolkit_db::secure::ScopeError::Denied(_),
                )),
            ) => {}
            other => panic!("unexpected dispatcher result: {other:?}"),
        }
        assert_eq!(calls.recv().await, None);
    }
}

#[tokio::test]
async fn dispatcher_shutdown_interrupts_database_backoff_before_another_attempt() {
    let (runtime, mut calls) = dispatch_runtime(vec![DispatchOutcome::Database], 60).await;
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task = tokio::spawn(async move { runtime.dispatch(task_stop).await });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), calls.recv())
            .await
            .unwrap(),
        Some(0)
    );
    // The authorization notification precedes returning the fault; cancellation
    // must work both before the backoff starts and while it is being awaited.
    tokio::task::yield_now().await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(calls.recv().await, None);
}
