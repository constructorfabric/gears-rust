use super::*;

fn disabled_configs() -> WorkerConfigs {
    WorkerConfigs {
        orphan_watchdog: OrphanWatchdogConfig {
            enabled: false,
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn all_workers_disabled_skips_leader_preflight() {
    let configs = disabled_configs();
    let elector = prepare_worker_runtime(&configs).await.unwrap();
    assert!(elector.is_none());

    let parent_cancel = CancellationToken::new();
    let (handles, worker_cancel) = spawn_workers::<
        crate::infra::db::repo::turn_repo::TurnRepository,
        crate::infra::db::repo::message_repo::MessageRepository,
    >(&configs, &parent_cancel, elector.as_ref(), None)
    .unwrap();
    assert_eq!(handles.len(), 0);

    worker_cancel.cancel();
    handles
        .join_all(CancellationToken::new(), Duration::from_millis(10))
        .await;
}

#[cfg(not(feature = "k8s"))]
#[tokio::test]
async fn leader_workers_preflight_with_noop_when_k8s_feature_is_disabled() {
    let mut configs = disabled_configs();
    configs.orphan_watchdog.enabled = true;

    let elector = prepare_worker_runtime(&configs).await.unwrap();
    assert!(elector.is_some());
}

#[cfg(feature = "k8s")]
#[tokio::test(flavor = "current_thread")]
async fn k8s_preflight_fails_without_required_env() {
    temp_env::async_with_vars(
        [("POD_NAMESPACE", None::<&str>), ("POD_NAME", None::<&str>)],
        async {
            let mut configs = disabled_configs();
            configs.orphan_watchdog.enabled = true;

            let err = prepare_worker_runtime(&configs).await.unwrap_err();
            assert!(
                err.to_string().contains("POD_NAMESPACE") || err.to_string().contains("POD_NAME")
            );
        },
    )
    .await;
}
