//! gRPC client implementation of Directory API
//!
//! This client allows remote gears to discover and resolve services via gRPC.

use anyhow::Result;
use async_trait::async_trait;
use tonic::transport::Channel;

use crate::api::{
    DirectoryClient, DirectoryNotFound, GrpcServiceInfo, RegisterInstanceInfo, ServiceEndpoint,
    ServiceInstanceInfo,
};
use toolkit_transport_grpc::client::{GrpcClientConfig, connect_with_retry};

use crate::{
    DeregisterInstanceRequest, DirectoryServiceClient, GrpcServiceEndpoint, HeartbeatRequest,
    ListInstancesRequest, RegisterInstanceRequest, ResolveGrpcServiceRequest,
    ResolveRestServiceRequest,
};

/// gRPC client for Directory API
///
/// This client connects to a remote `DirectoryService` via gRPC and provides
/// typed access to service discovery functionality. It includes:
/// - Configurable timeouts and retries via transport stack
/// - Automatic proto ↔ domain type conversions
/// - Distributed tracing and metrics
pub struct DirectoryGrpcClient {
    inner: DirectoryServiceClient<Channel>,
}

impl DirectoryGrpcClient {
    /// Connect to a directory service using default configuration with retries.
    ///
    /// Uses exponential backoff retry logic for reliable connection establishment.
    /// This is the recommended method for `OoP` gears connecting to the master host.
    ///
    /// # Errors
    /// It will return an error when it fails
    pub async fn connect(uri: impl Into<String>) -> Result<Self> {
        let cfg = GrpcClientConfig::new("directory");
        Self::connect_with_retry(uri, &cfg).await
    }

    /// Connect to a directory service with custom configuration and retry logic.
    ///
    /// Uses exponential backoff based on `cfg.max_retries`, `cfg.base_backoff`,
    /// and `cfg.max_backoff` settings.
    ///
    /// # Errors
    /// It will return an error when it fails
    pub async fn connect_with_retry(
        uri: impl Into<String>,
        cfg: &GrpcClientConfig,
    ) -> Result<Self> {
        let channel: Channel = connect_with_retry(uri, cfg).await?;
        Ok(Self {
            inner: DirectoryServiceClient::new(channel),
        })
    }

    /// Connect to a directory service without retry logic.
    ///
    /// This method attempts a single connection. Use `connect` or `connect_with_retry`
    /// for production scenarios where the directory service may not be immediately available.
    ///
    /// # Errors
    /// It will return an error when it fails
    pub async fn connect_no_retry(uri: impl Into<String>, cfg: &GrpcClientConfig) -> Result<Self> {
        let uri_string = uri.into();

        // Create endpoint with timeouts from config
        let endpoint = tonic::transport::Endpoint::from_shared(uri_string)?
            .connect_timeout(cfg.connect_timeout)
            .timeout(cfg.rpc_timeout);

        // Connect to the service
        let channel = endpoint.connect().await?;

        if cfg.enable_tracing {
            tracing::debug!(
                service_name = cfg.service_name,
                connect_timeout_ms = cfg.connect_timeout.as_millis(),
                rpc_timeout_ms = cfg.rpc_timeout.as_millis(),
                "directory gRPC client connected"
            );
        }

        Ok(Self {
            inner: DirectoryServiceClient::new(channel),
        })
    }

    /// Create from an existing channel (useful for testing or custom setup)
    #[must_use]
    pub fn from_channel(channel: Channel) -> Self {
        Self {
            inner: DirectoryServiceClient::new(channel),
        }
    }
}

#[async_trait]
impl DirectoryClient for DirectoryGrpcClient {
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        let mut client = self.inner.clone();
        let request = tonic::Request::new(ResolveGrpcServiceRequest {
            service_name: service_name.to_owned(),
        });

        let response = client
            .resolve_grpc_service(request)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC call failed: {e}"))?;

        let proto_response = response.into_inner();
        if proto_response.endpoint_uri.is_empty() {
            return Err(anyhow::Error::new(DirectoryNotFound::new(format!(
                "service {service_name}"
            ))));
        }
        Ok(ServiceEndpoint::new(proto_response.endpoint_uri))
    }

    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
        let mut client = self.inner.clone();
        let request = tonic::Request::new(ResolveRestServiceRequest {
            gear_name: gear_name.to_owned(),
        });

        let response = client
            .resolve_rest_service(request)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC call failed: {e}"))?;

        let proto_response = response.into_inner();
        if proto_response.endpoint_uri.is_empty() {
            return Err(anyhow::Error::new(DirectoryNotFound::new(format!(
                "gear {gear_name}"
            ))));
        }
        Ok(ServiceEndpoint::new(proto_response.endpoint_uri))
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        let mut client = self.inner.clone();
        let request = tonic::Request::new(ListInstancesRequest {
            gear_name: gear.to_owned(),
        });

        let response = client
            .list_instances(request)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC call failed: {e}"))?;

        let proto_response = response.into_inner();

        // Convert proto instances to domain types
        let instances = proto_response
            .instances
            .into_iter()
            .map(|proto_inst| ServiceInstanceInfo {
                gear: proto_inst.gear_name,
                instance_id: proto_inst.instance_id,
                grpc_services: proto_inst
                    .grpc_services
                    .into_iter()
                    .map(|svc| GrpcServiceInfo {
                        service_name: svc.service_name,
                        endpoint: ServiceEndpoint::new(svc.endpoint_uri),
                    })
                    .collect(),
                version: if proto_inst.version.is_empty() {
                    None
                } else {
                    Some(proto_inst.version)
                },
                rest_endpoint: proto_inst
                    .rest_endpoint_uri
                    .filter(|uri| !uri.is_empty())
                    .map(ServiceEndpoint::new),
            })
            .collect();

        Ok(instances)
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        let mut client = self.inner.clone();

        // Convert gRPC service endpoints
        let grpc_services = info
            .grpc_services
            .into_iter()
            .map(|service| GrpcServiceEndpoint {
                service_name: service.service_name,
                endpoint_uri: service.endpoint.uri,
            })
            .collect();

        let req = RegisterInstanceRequest {
            gear_name: info.gear,
            instance_id: info.instance_id,
            grpc_services,
            version: info.version.unwrap_or_default(),
            rest_endpoint_uri: info.rest_endpoint.map(|ep| ep.uri),
        };

        client
            .register_instance(tonic::Request::new(req))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC register_instance failed: {e}"))?;

        Ok(())
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let mut client = self.inner.clone();

        let req = DeregisterInstanceRequest {
            gear_name: gear.to_owned(),
            instance_id: instance_id.to_owned(),
        };

        client
            .deregister_instance(tonic::Request::new(req))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC deregister_instance failed: {e}"))?;

        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        let mut client = self.inner.clone();

        let req = HeartbeatRequest {
            gear_name: gear.to_owned(),
            instance_id: instance_id.to_owned(),
        };

        client
            .heartbeat(tonic::Request::new(req))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC heartbeat failed: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grpc_client_can_be_constructed() {
        // Smoke test to ensure types compile and connect
        let endpoint = tonic::transport::Endpoint::from_static("http://[::1]:50051");

        // We can't actually connect without a server, but we can construct the client type
        // This ensures the API is correct
        let channel_result = endpoint.connect().await;

        // It's expected to fail since there's no server, but if it does somehow succeed:
        if let Ok(channel) = channel_result {
            let _client = DirectoryGrpcClient::from_channel(channel);
        }
    }
}
