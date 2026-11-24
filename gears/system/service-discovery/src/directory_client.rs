use crate::directory_backend::{DirectoryBackend, DirectoryBackendError};
use crate::local_directory_client::{
    LocalDirectoryClient, conflict_error, not_found, parse_instance_id, registered_instance,
};
use crate::replica::DirectoryReplica;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;
use toolkit::directory::{
    DirectoryClient, LabelSelector, RegisterAck, RegisterInstanceInfo, ReportedState,
    ServiceEndpoint, ServiceInstanceInfo,
};
use uuid::Uuid;

/// [`DirectoryClient`] for a replica sharing a [`DirectoryBackend`]: writes go
/// through the backend and are then applied to the replica's read model;
/// reads are served from the read model.
pub struct ReplicatedDirectoryClient {
    local: LocalDirectoryClient,
    replica: Arc<DirectoryReplica>,
    backend: Arc<dyn DirectoryBackend>,
}

impl ReplicatedDirectoryClient {
    pub fn new(replica: Arc<DirectoryReplica>, backend: Arc<dyn DirectoryBackend>) -> Self {
        Self {
            local: LocalDirectoryClient::new(Arc::clone(replica.manager())),
            replica,
            backend,
        }
    }
}

fn backend_error(
    operation: &'static str,
    gear: &str,
    instance_id: Uuid,
    err: DirectoryBackendError,
) -> anyhow::Error {
    match err {
        DirectoryBackendError::Conflict(conflict) => conflict_error(conflict),
        DirectoryBackendError::Unavailable(err) => {
            tracing::error!(
                operation,
                gear,
                instance_id = %instance_id,
                error = format!("{err:#}"),
                "directory backend operation failed"
            );
            anyhow::anyhow!("directory backend unavailable")
        }
    }
}

#[async_trait]
impl DirectoryClient for ReplicatedDirectoryClient {
    async fn resolve_grpc_service(&self, name: &str) -> Result<ServiceEndpoint> {
        self.local.resolve_grpc_service(name).await
    }

    async fn resolve_rest_service(&self, gear: &str) -> Result<ServiceEndpoint> {
        self.local.resolve_rest_service(gear).await
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        self.local.list_instances(gear).await
    }

    async fn resolve_by_labels(
        &self,
        gear: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<ServiceInstanceInfo>> {
        self.local.resolve_by_labels(gear, selector).await
    }

    async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
        self.local.list_all_instances().await
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        let instance = Arc::new(registered_instance(info)?);
        let manager = self.replica.manager();
        manager
            .check_pinned_grpc_service_ownership(&instance)
            .map_err(conflict_error)?;
        let (gear, id) = (instance.gear.clone(), instance.instance_id);
        let started = Instant::now();
        let record = self
            .backend
            .register(instance, manager.lease_ttl(), manager.lease_grace())
            .await
            .map_err(|error| backend_error("register", &gear, id, error))?;
        self.replica
            .apply_write(record, started)
            .map_err(conflict_error)
    }

    async fn register_with_lease(&self, info: RegisterInstanceInfo) -> Result<RegisterAck> {
        self.register_instance(info).await?;
        Ok(RegisterAck {
            lease_ttl: Some(self.replica.manager().lease_ttl()),
        })
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let id = parse_instance_id(instance_id)?;
        if let Some(record) = self
            .backend
            .deregister(gear, id)
            .await
            .map_err(|error| backend_error("deregister", gear, id, error))?
        {
            self.replica.apply_removal(gear, id, record.generation);
        }
        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        self.heartbeat(gear, instance_id, None).await
    }

    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: &str,
        reported: Option<ReportedState>,
    ) -> Result<()> {
        let id = parse_instance_id(instance_id)?;
        let reported = reported.unwrap_or(ReportedState::Serving).into();
        let manager = self.replica.manager();
        let started = Instant::now();
        let Some(record) = self
            .backend
            .heartbeat(
                gear,
                id,
                reported,
                manager.lease_ttl(),
                manager.lease_grace(),
            )
            .await
            .map_err(|error| backend_error("heartbeat", gear, id, error))?
        else {
            return Err(not_found(gear, instance_id));
        };
        self.replica
            .apply_write(record, started)
            .map_err(conflict_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory_backend::{
        DirectoryChangeStream, DirectoryRecord, MemoryDirectoryBackend,
    };
    use std::time::Duration;
    use toolkit::directory::DirectoryServiceNameConflict;
    use toolkit::runtime::{
        Endpoint, GearInstance, GearManager, ReportedState as RuntimeReportedState,
    };

    fn failure() -> DirectoryBackendError {
        DirectoryBackendError::Unavailable(anyhow::anyhow!("distinctive secret backend failure"))
    }

    fn client(
        manager: Arc<GearManager>,
        backend: Arc<dyn DirectoryBackend>,
    ) -> ReplicatedDirectoryClient {
        ReplicatedDirectoryClient::new(Arc::new(DirectoryReplica::new(manager)), backend)
    }

    struct FailingBackend;

    #[async_trait]
    impl DirectoryBackend for FailingBackend {
        async fn register(
            &self,
            _instance: Arc<GearInstance>,
            _ttl: Duration,
            _grace: Duration,
        ) -> Result<DirectoryRecord, DirectoryBackendError> {
            Err(failure())
        }

        async fn heartbeat(
            &self,
            _gear: &str,
            _instance_id: Uuid,
            _reported: RuntimeReportedState,
            _ttl: Duration,
            _grace: Duration,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            Err(failure())
        }

        async fn deregister(
            &self,
            _gear: &str,
            _instance_id: Uuid,
        ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
            Err(failure())
        }

        async fn load_all(
            &self,
            _grace: Duration,
        ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
            Ok(Vec::new())
        }

        async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }

    #[tokio::test]
    async fn heartbeat_before_feed_and_stale_state_are_applied_atomically() {
        let backend = Arc::new(MemoryDirectoryBackend::default());
        let manager_a = Arc::new(GearManager::new());
        let manager_b = Arc::new(GearManager::new());
        let client_a = client(manager_a, backend.clone());
        let client_b = client(Arc::clone(&manager_b), backend.clone());
        let id = Uuid::new_v4();

        client_a
            .register_instance(
                RegisterInstanceInfo::new("shared", id.to_string())
                    .with_reported_state(ReportedState::Starting),
            )
            .await
            .unwrap();
        assert!(manager_b.instances_of("shared").is_empty());

        // The registration has not reached this replica's read model yet.
        // Heartbeat must consult the backend rather than return NOT_FOUND.
        client_b
            .heartbeat("shared", &id.to_string(), Some(ReportedState::Starting))
            .await
            .unwrap();
        assert_eq!(
            manager_b.instances_of("shared")[0].state(),
            toolkit::runtime::InstanceState::Registered
        );

        client_a
            .heartbeat("shared", &id.to_string(), Some(ReportedState::Serving))
            .await
            .unwrap();
        let authoritative = backend.load_all(Duration::from_secs(30)).await.unwrap();
        assert_eq!(
            authoritative[0].instance.reported_state,
            Some(RuntimeReportedState::Serving)
        );

        // This replica has not received the Serving feed event. Its stale
        // reported state must be applied atomically with the renewed lease.
        client_b
            .heartbeat("shared", &id.to_string(), Some(ReportedState::Starting))
            .await
            .unwrap();
        let authoritative = backend.load_all(Duration::from_secs(30)).await.unwrap();
        assert_eq!(
            authoritative[0].instance.reported_state,
            Some(RuntimeReportedState::Starting)
        );
        assert_eq!(
            manager_b.instances_of("shared")[0].state(),
            toolkit::runtime::InstanceState::Registered
        );
    }

    #[tokio::test]
    async fn backend_diagnostics_are_logged_but_not_returned_to_callers() {
        let client = client(Arc::new(GearManager::new()), Arc::new(FailingBackend));
        let id = Uuid::new_v4().to_string();

        let failures = [
            client
                .register_instance(RegisterInstanceInfo::new("gear", id.clone()))
                .await
                .unwrap_err(),
            client
                .heartbeat("gear", &id, Some(ReportedState::Serving))
                .await
                .unwrap_err(),
            client.deregister_instance("gear", &id).await.unwrap_err(),
        ];
        for error in failures {
            assert_eq!(error.to_string(), "directory backend unavailable");
            assert!(!format!("{error:#}").contains("distinctive secret"));
        }
    }

    #[tokio::test]
    async fn backend_claim_conflict_is_returned_as_a_typed_conflict() {
        let backend = Arc::new(MemoryDirectoryBackend::default());
        backend
            .register(
                Arc::new(
                    GearInstance::new("owner", Uuid::new_v4())
                        .with_grpc_service("claimed.Service", Endpoint::http("127.0.0.1", 9000)),
                ),
                Duration::from_secs(30),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        let client = client(Arc::new(GearManager::new()), backend);

        let error = client
            .register_instance(
                RegisterInstanceInfo::new("squatter", Uuid::new_v4().to_string())
                    .with_grpc_services(vec![(
                        "claimed.Service".to_owned(),
                        ServiceEndpoint::new("http://127.0.0.1:9001"),
                    )]),
            )
            .await
            .unwrap_err();

        let conflict = error
            .downcast_ref::<DirectoryServiceNameConflict>()
            .expect("a backend claim conflict must surface as DirectoryServiceNameConflict");
        assert_eq!(conflict.owner, "owner");
        assert!(conflict.recoverable);
    }
}
