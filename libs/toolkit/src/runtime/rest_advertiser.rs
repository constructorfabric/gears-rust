use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Advertise each embedded REST provider with one bounded retry loop.
/// Routes become discoverable per startup group. `OoP` serving owns its own presence.
pub(super) struct RestAdvertiser {
    instance_id: String,
    cancel: CancellationToken,
    state: parking_lot::Mutex<AdvertiserState>,
}

#[derive(Default)]
struct AdvertiserState {
    target: Option<(Arc<dyn crate::DirectoryClient>, String)>,
    providers: Vec<&'static str>,
    tasks: Vec<tokio_util::task::AbortOnDropHandle<()>>,
}

impl RestAdvertiser {
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    pub(super) fn new(instance_id: Uuid, owned: &CancellationToken) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            cancel: owned.child_token(),
            state: parking_lot::Mutex::new(AdvertiserState::default()),
        }
    }

    pub(super) fn enable(&self, dir: Arc<dyn crate::DirectoryClient>, endpoint: String) {
        if !self.cancel.is_cancelled() {
            self.state.lock().target = Some((dir, endpoint));
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.state.lock().target.is_some()
    }

    /// Await one bounded attempt, then retry in the background during admission.
    pub(super) async fn advertise(&self, gear: &'static str) {
        let first = {
            let mut state = self.state.lock();
            let Some((dir, endpoint)) = state.target.clone() else {
                return;
            };
            if self.cancel.is_cancelled() || state.providers.contains(&gear) {
                return;
            }
            // Deregister uncertain writes too: the server may have accepted them.
            state.providers.push(gear);
            let (first, done) = tokio::sync::oneshot::channel();
            let instance_id = self.instance_id.clone();
            let cancel = self.cancel.clone();
            let task = tokio::spawn(Self::register_until_accepted(
                dir,
                endpoint,
                instance_id,
                gear,
                cancel,
                first,
            ));
            state
                .tasks
                .push(tokio_util::task::AbortOnDropHandle::new(task));
            done
        };
        // A cancelled/dropped runtime also drops the sender and releases startup.
        first.await.unwrap_or_default();
    }

    async fn register_until_accepted(
        dir: Arc<dyn crate::DirectoryClient>,
        endpoint: String,
        instance_id: String,
        gear: &'static str,
        cancel: CancellationToken,
        first: tokio::sync::oneshot::Sender<()>,
    ) {
        let mut first = Some(first);
        let mut backoff = std::time::Duration::from_millis(100);
        while !cancel.is_cancelled() {
            // Finish the bounded write before deregistration to prevent late re-registration.
            let result = tokio::time::timeout(
                Self::ATTEMPT_TIMEOUT,
                Self::register_once(&dir, &endpoint, &instance_id, gear, &cancel),
            )
            .await
            .map_err(anyhow::Error::from)
            .and_then(std::convert::identity);
            if let Some(first) = first.take() {
                first.send(()).unwrap_or_default();
            }
            match result {
                Ok(()) => return,
                Err(error) => {
                    tracing::warn!(gear, %error, "directory-register: registration attempt failed");
                    if error.is::<cf_system_sdks::directory::DirectoryInvalidArgument>()
                        || error.is::<cf_system_sdks::directory::DirectoryPermissionDenied>()
                    {
                        return;
                    }
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = backoff
                .saturating_mul(2)
                .min(std::time::Duration::from_secs(30));
        }
    }

    async fn register_once(
        dir: &Arc<dyn crate::DirectoryClient>,
        endpoint: &str,
        instance_id: &str,
        gear: &'static str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        // Preserve gRPC metadata for the same (gear, instance_id).
        // Empty labels preserve stored labels atomically; read-modify-write
        // could overwrite concurrent label changes.
        let (grpc_services, version) = match dir.list_instances(gear).await {
            Ok(insts) => insts
                .into_iter()
                .find(|i| i.instance_id == instance_id)
                .map(|i| (i.grpc_services, i.version))
                .unwrap_or_default(),
            Err(e) => {
                // Warn before falling back to registration without prior metadata.
                tracing::warn!(
                    gear,
                    error = %e,
                    "directory-register: failed to read existing registration; \
                     re-registering with empty grpc_services/version"
                );
                (Vec::new(), None)
            }
        };
        // grpc-hub publishes OpenAPI separately; omit labels to preserve them.
        if cancel.is_cancelled() {
            return Ok(());
        }
        let mut info = crate::RegisterInstanceInfo::new(gear, instance_id)
            .with_grpc_services(grpc_services)
            .with_rest_endpoint(crate::ServiceEndpoint::new(endpoint));
        if let Some(version) = version {
            info = info.with_version(version);
        }
        dir.register_instance(info).await?;
        tracing::info!(gear, endpoint, "registered REST provider in directory");
        Ok(())
    }

    /// Deregister every advertised gear and disable further advertisement.
    pub(super) async fn deregister_all(&self) {
        self.cancel.cancel();
        let (target, gears, tasks) = {
            let mut state = self.state.lock();
            (
                state.target.take(),
                std::mem::take(&mut state.providers),
                std::mem::take(&mut state.tasks),
            )
        };
        for task in tasks {
            if let Err(error) = task.await {
                tracing::warn!(%error, "directory-register: registration task failed during shutdown");
            }
        }
        let Some((dir, _)) = target else {
            return;
        };
        for gear in gears {
            match tokio::time::timeout(
                Self::ATTEMPT_TIMEOUT,
                dir.deregister_instance(gear, &self.instance_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    gear,
                    %error,
                    "directory-deregister: failed to deregister REST provider"
                ),
                Err(_) => tracing::warn!(
                    gear,
                    "directory-deregister: timed out deregistering REST provider"
                ),
            }
        }
    }
}
