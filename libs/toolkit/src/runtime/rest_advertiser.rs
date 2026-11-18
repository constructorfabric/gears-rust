use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::runtime::PresenceManager;

/// Advertise each embedded REST provider with one bounded retry loop.
/// Routes become discoverable per startup group. `OoP` serving owns its own presence.
///
/// Registration goes through the process [`PresenceManager`], which then owns
/// lease renewal for each advertised record: this advertiser writes
/// the record and deregisters it on shutdown, but never heartbeats.
pub(super) struct RestAdvertiser {
    instance_id: String,
    cancel: CancellationToken,
    state: parking_lot::Mutex<AdvertiserState>,
}

#[derive(Default)]
struct AdvertiserState {
    target: Option<Target>,
    providers: Vec<&'static str>,
    tasks: Vec<tokio_util::task::AbortOnDropHandle<()>>,
}

/// Where this advertiser registers: the presence manager that renews the
/// records, the directory (for reading existing metadata and for deregister),
/// and the REST endpoint to advertise.
#[derive(Clone)]
struct Target {
    presence: Arc<PresenceManager>,
    directory: Arc<dyn crate::DirectoryClient>,
    endpoint: String,
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

    pub(super) fn enable(
        &self,
        presence: Arc<PresenceManager>,
        directory: Arc<dyn crate::DirectoryClient>,
        endpoint: String,
    ) {
        if !self.cancel.is_cancelled() {
            self.state.lock().target = Some(Target {
                presence,
                directory,
                endpoint,
            });
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.state.lock().target.is_some()
    }

    /// Await one bounded attempt, then retry in the background during admission.
    pub(super) async fn advertise(&self, gear: &'static str) {
        let first = {
            let mut state = self.state.lock();
            let Some(target) = state.target.clone() else {
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
                target,
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
        target: Target,
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
                Self::register_once(&target, &instance_id, gear, &cancel),
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
        target: &Target,
        instance_id: &str,
        gear: &'static str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        let dir = &target.directory;
        let endpoint = &target.endpoint;
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
            .with_rest_endpoint(crate::ServiceEndpoint::new(endpoint.clone()));
        if let Some(version) = version {
            info = info.with_version(version);
        }
        // Register through the presence manager, which tracks the record and
        // owns its lease renewal from here on; this advertiser never heartbeats.
        target.presence.register(info).await?;
        tracing::info!(
            gear,
            endpoint,
            "registered REST provider via presence manager"
        );
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
        let Some(target) = target else {
            return;
        };
        for gear in gears {
            // Stop the presence manager renewing this record before removing it,
            // so its renewal loop cannot re-register it after deregistration.
            target.presence.untrack(gear, &self.instance_id);
            match tokio::time::timeout(
                Self::ATTEMPT_TIMEOUT,
                target
                    .directory
                    .deregister_instance(gear, &self.instance_id),
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
