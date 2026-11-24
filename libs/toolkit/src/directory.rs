//! Directory API contract re-exports, conversions to and from the runtime
//! read model, and `OpenAPI` hashing.

/// Compute the lowercase-hex SHA-256 of an `OpenAPI` document.
///
/// This is the stable content hash the worker advertises and the edge verifies
/// (ADR-0011, "`OpenAPI` document"). Unlike the former `DefaultHasher` token, it
/// is stable across binaries, so a worker and the edge compute the same value
/// for the same document.
#[must_use]
pub fn openapi_spec_hash(spec: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = toolkit_security::crypto::hash::Sha256::digest(spec);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    hex
}

pub use cf_system_sdks::directory::{
    DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, DirectoryPermissionDenied,
    DirectoryServiceNameConflict, GrpcServiceInfo, InstanceState, LabelSelector, RegisterAck,
    RegisterInstanceInfo, ReportedState, ServiceEndpoint, ServiceInstanceInfo,
};

use crate::runtime::{
    Endpoint, GearInstance, GrpcServiceNameConflict, InstanceState as RuntimeInstanceState,
    ReportedState as RuntimeReportedState,
};
use uuid::Uuid;

impl From<ReportedState> for RuntimeReportedState {
    fn from(state: ReportedState) -> Self {
        match state {
            ReportedState::Starting => Self::Starting,
            ReportedState::Serving => Self::Serving,
            ReportedState::Draining => Self::Draining,
        }
    }
}

impl From<RuntimeInstanceState> for InstanceState {
    fn from(state: RuntimeInstanceState) -> Self {
        match state {
            RuntimeInstanceState::Registered => Self::Registered,
            RuntimeInstanceState::Ready => Self::Ready,
            RuntimeInstanceState::Healthy => Self::Healthy,
            RuntimeInstanceState::Quarantined => Self::Quarantined,
            RuntimeInstanceState::Draining => Self::Draining,
        }
    }
}

impl From<GrpcServiceNameConflict> for DirectoryServiceNameConflict {
    fn from(conflict: GrpcServiceNameConflict) -> Self {
        Self {
            service_name: conflict.service_name,
            owner: conflict.owner,
            recoverable: conflict.recoverable,
        }
    }
}

/// Build the [`GearInstance`] a `RegisterInstance` request describes, with
/// `instance_id` already parsed from `info.instance_id`.
pub fn instance_from_registration(info: RegisterInstanceInfo, instance_id: Uuid) -> GearInstance {
    let mut instance = GearInstance::new(info.gear, instance_id);
    if let Some(version) = info.version {
        instance = instance.with_version(version);
    }
    for (service_name, endpoint) in info.grpc_services {
        instance = instance.with_grpc_service(service_name, Endpoint::from_uri(endpoint.uri));
    }
    if let Some(endpoint) = info.rest_endpoint {
        instance = instance.with_rest_endpoint(Endpoint::from_uri(endpoint.uri));
    }
    if let Some(hash) = info.openapi_spec_hash {
        instance = instance.with_openapi_spec_hash(hash);
    }
    if !info.labels.is_empty() {
        instance = instance.with_labels(info.labels);
    }
    if let Some(reported) = info.reported_state {
        instance = instance.with_reported_state(reported.into());
    }
    instance
}

/// Project a stored [`GearInstance`] onto the SDK's [`ServiceInstanceInfo`],
/// with its current observable state.
#[must_use]
pub fn project_instance(instance: &GearInstance) -> ServiceInstanceInfo {
    let endpoint = instance
        .grpc_services
        .values()
        .next()
        .or(instance.rest_endpoint.as_ref())
        .map(|endpoint| ServiceEndpoint::new(endpoint.uri.clone()));

    ServiceInstanceInfo::new(&instance.gear, instance.instance_id.to_string())
        .with_endpoint(endpoint)
        .with_version(instance.version.clone())
        .with_rest_endpoint(
            instance
                .rest_endpoint
                .as_ref()
                .map(|endpoint| ServiceEndpoint::new(endpoint.uri.clone())),
        )
        .with_openapi_spec_hash(instance.openapi_spec_hash.clone())
        .with_grpc_services(
            instance
                .grpc_services
                .iter()
                .map(|(name, endpoint)| (name.clone(), ServiceEndpoint::new(endpoint.uri.clone())))
                .collect(),
        )
        .with_labels(instance.labels.clone())
        .with_state(instance.state().into())
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::{
        DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, DirectoryServiceNameConflict,
        LabelSelector, RegisterAck, RegisterInstanceInfo, ReportedState, ServiceEndpoint,
        ServiceInstanceInfo, instance_from_registration, project_instance,
    };
    use crate::runtime::GearManager;

    /// Test-only in-process `DirectoryClient` backed by a `GearManager`.
    ///
    /// Runtime code should use the adapter owned by the service-discovery gear.
    pub struct ManagerDirectoryClient(Arc<GearManager>);

    impl ManagerDirectoryClient {
        /// Create a test client backed by `manager`.
        #[must_use]
        pub fn new(manager: Arc<GearManager>) -> Self {
            Self(manager)
        }
    }

    fn registration_id(instance_id: &str) -> Result<Uuid> {
        Uuid::parse_str(instance_id).map_err(|error| {
            anyhow::Error::from(DirectoryInvalidArgument::new(format!(
                "invalid instance_id '{instance_id}': {error}"
            )))
        })
    }

    fn not_found(gear: &str, instance_id: &str) -> anyhow::Error {
        DirectoryNotFound::new(format!("instance {gear}/{instance_id}")).into()
    }

    #[async_trait]
    impl DirectoryClient for ManagerDirectoryClient {
        async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
            self.0
                .pick_service_round_robin(service_name)
                .map(|(_, _, endpoint)| ServiceEndpoint::new(endpoint.uri))
                .ok_or_else(|| DirectoryNotFound::new(format!("service {service_name}")).into())
        }

        async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
            self.0
                .pick_rest_endpoint_round_robin(gear_name)
                .map(|endpoint| ServiceEndpoint::new(endpoint.uri))
                .ok_or_else(|| DirectoryNotFound::new(format!("gear {gear_name}")).into())
        }

        async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
            Ok(self
                .0
                .instances_of(gear)
                .iter()
                .map(|instance| project_instance(instance))
                .collect())
        }

        async fn resolve_by_labels(
            &self,
            gear: &str,
            selector: &LabelSelector,
        ) -> Result<Vec<ServiceInstanceInfo>> {
            Ok(self
                .0
                .instances_of(gear)
                .iter()
                .map(|instance| project_instance(instance))
                .filter(|instance| selector.matches(&instance.labels))
                .collect())
        }

        async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
            Ok(self
                .0
                .all_instances()
                .iter()
                .filter_map(|instance| {
                    let info = project_instance(instance);
                    info.endpoint.is_some().then(|| info.without_labels())
                })
                .collect())
        }

        async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
            cf_system_sdks::directory::validate_labels(&info.labels).map_err(|error| {
                anyhow::Error::from(DirectoryInvalidArgument::new(error.to_string()))
            })?;
            let id = registration_id(&info.instance_id)?;
            self.0
                .register_instance(Arc::new(instance_from_registration(info, id)))
                .map_err(|conflict| DirectoryServiceNameConflict::from(conflict).into())
        }

        async fn register_with_lease(&self, info: RegisterInstanceInfo) -> Result<RegisterAck> {
            self.register_instance(info).await?;
            Ok(RegisterAck {
                lease_ttl: Some(self.0.lease_ttl()),
            })
        }

        async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
            let id = registration_id(instance_id)?;
            self.0.deregister(gear, id);
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
            let id = registration_id(instance_id)?;
            if self.0.heartbeat(gear, id, reported.map(Into::into)) {
                Ok(())
            } else {
                Err(not_found(gear, instance_id))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectoryClient, DirectoryInvalidArgument, openapi_spec_hash};
    use crate::directory::test_support::ManagerDirectoryClient;
    use crate::runtime::GearManager;
    use std::sync::Arc;

    #[test]
    fn openapi_hash_is_sha256_hex() {
        assert_eq!(
            openapi_spec_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn manager_directory_client_uses_directory_invalid_argument_for_bad_request_ids() {
        let client = ManagerDirectoryClient::new(Arc::new(GearManager::new()));

        assert!(matches!(
            client.deregister_instance("gear", "not-a-uuid").await,
            Err(error) if error.is::<DirectoryInvalidArgument>()
        ));
        assert!(matches!(
            client.heartbeat("gear", "not-a-uuid", None).await,
            Err(error) if error.is::<DirectoryInvalidArgument>()
        ));
    }
}
