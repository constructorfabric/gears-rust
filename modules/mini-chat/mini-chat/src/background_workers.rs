use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::OrphanWatchdogConfig;
use crate::domain::repos::{MessageRepository, TurnRepository};
use crate::infra::leader::LeaderElector;
use crate::infra::workers::WorkerHandles;
use crate::infra::workers::orphan_watchdog::OrphanWatchdogDeps;

/// Worker configs captured in `init()` and consumed by `start()`.
pub struct WorkerConfigs {
    pub(crate) orphan_watchdog: OrphanWatchdogConfig,
}

/// Default grace period for top-level worker shutdown before remaining tasks
/// are aborted.
pub const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Preflight worker runtime requirements before starting any background work.
///
/// This currently validates whether a leader elector can be constructed when
/// leader-only workers are enabled.
pub async fn prepare_worker_runtime(
    configs: &WorkerConfigs,
) -> anyhow::Result<Option<Arc<dyn LeaderElector>>> {
    if leader_workers_enabled(configs) {
        Ok(Some(create_leader_elector().await?))
    } else {
        Ok(None)
    }
}

/// Spawn all background workers, returning their handles and a cancellation
/// token that can be used to request shutdown.
pub fn spawn_workers<TR, MR>(
    configs: &WorkerConfigs,
    parent_cancel: &CancellationToken,
    leader_elector: Option<&Arc<dyn LeaderElector>>,
    orphan_deps: Option<OrphanWatchdogDeps<TR, MR>>,
) -> anyhow::Result<(WorkerHandles, CancellationToken)>
where
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
{
    let worker_cancel = parent_cancel.child_token();
    let mut handles = WorkerHandles::new();

    if configs.orphan_watchdog.enabled {
        let elector = Arc::clone(
            leader_elector
                .ok_or_else(|| anyhow::anyhow!("leader elector required for orphan_watchdog"))?,
        );
        let deps = orphan_deps
            .ok_or_else(|| anyhow::anyhow!("orphan watchdog deps required when enabled"))?;
        let cancel = worker_cancel.child_token();
        handles.spawn(
            "orphan_watchdog",
            cancel.clone(),
            crate::infra::workers::orphan_watchdog::run(
                elector,
                configs.orphan_watchdog.clone(),
                deps,
                cancel,
            ),
        );
    }

    Ok((handles, worker_cancel))
}

fn leader_workers_enabled(configs: &WorkerConfigs) -> bool {
    configs.orphan_watchdog.enabled
}

/// Create the appropriate [`LeaderElector`] based on compile-time features
/// and runtime environment.
///
/// When built with `k8s`, mini-chat requires Kubernetes runtime support:
/// `POD_NAMESPACE`, `POD_NAME`, and kube client access must all be available.
#[allow(
    clippy::unused_async,
    reason = "async needed when k8s feature is enabled"
)]
async fn create_leader_elector() -> anyhow::Result<Arc<dyn LeaderElector>> {
    #[cfg(feature = "k8s")]
    {
        use crate::infra::leader::k8s_lease::{K8sLeaseConfig, K8sLeaseElector};
        use anyhow::Context;

        let config = K8sLeaseConfig::from_env("mini-chat")
            .context("k8s feature enabled: POD_NAMESPACE and POD_NAME are required")?;
        let elector = K8sLeaseElector::from_default(config).await.context(
            "k8s feature enabled: kube client init failed; Kubernetes runtime access is required",
        )?;
        info!("Using K8s Lease leader election");
        Ok(Arc::new(elector))
    }
    #[cfg(not(feature = "k8s"))]
    {
        info!("Using noop leader election (single-process mode)");
        Ok(crate::infra::leader::noop())
    }
}
#[cfg(test)]
#[path = "background_workers_tests.rs"]
mod tests;
