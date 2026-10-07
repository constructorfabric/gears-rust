use super::*;
#[tokio::test]
async fn runtime_delivers_outbox_and_completes_all_checkpoints() {
    let url = crate::test_postgres::database().await;
    let store = store().await;
    let j = journal();
    let id = j.run.id;
    let scope = AccessScope::allow_all();
    store.insert(scope.clone(), j).await.unwrap();
    let registry = Arc::new(crate::domain::registry::Registry::default());
    registry.register(definition()).unwrap();
    let executor = Arc::new(crate::infra::executor::Executor {
        store: store.clone(),
        registry,
        config: crate::config::Config {
            execute_activities: true,
            service_client_id: "test-worker".into(),
            dispatch_interval_secs: 1,
            ..Default::default()
        },
    });
    let runtime = crate::infra::runtime::Runtime::prepare(executor, &url)
        .await
        .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let worker_stop = stop.clone();
    let worker = tokio::spawn(async move { runtime.run(worker_stop).await });
    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = store.get(&scope, id).await.unwrap().unwrap();
            if current.run.status == RunStatus::Succeeded {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let completed = completed.unwrap();
    assert!(
        completed
            .run
            .activities
            .iter()
            .all(|a| a.attempts == 1 && a.status == ActivityStatus::Succeeded)
    );
}
