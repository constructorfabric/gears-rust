//! gRPC server implementation for `DirectoryService`
//!
//! This gear provides the gRPC service implementation for Directory Service.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use cf_system_sdks::directory::{
    DeregisterInstanceRequest, DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound,
    DirectoryService, DirectoryServiceServer, HeartbeatRequest, InstanceInfo, ListInstancesRequest,
    ListInstancesResponse, RegisterInstanceInfo, RegisterInstanceRequest,
    ResolveGrpcServiceRequest, ResolveGrpcServiceResponse, ResolveRestServiceRequest,
    ResolveRestServiceResponse, ServiceEndpoint,
};

/// Map an `anyhow::Error` from the directory API into a `tonic::Status`.
/// Genuine "not found" misses (signalled by the [`DirectoryNotFound`]
/// sentinel) become `Status::not_found`; everything else is treated as an
/// internal failure of the directory implementation. Sanitises the message
/// — only the public portion of the error chain reaches the client.
fn map_directory_error(err: &anyhow::Error) -> Status {
    if let Some(missing) = err.downcast_ref::<DirectoryNotFound>() {
        return Status::not_found(missing.to_string());
    }
    if let Some(bad) = err.downcast_ref::<DirectoryInvalidArgument>() {
        return Status::invalid_argument(bad.message.clone());
    }
    // Treat anything else as a transient/internal failure; the raw error is
    // logged server-side, but the wire message stays generic so we don't
    // leak DB connection strings, file paths, or upstream stack traces.
    tracing::warn!(error = %err, "directory operation failed");
    Status::internal("directory operation failed")
}

/// gRPC service implementation of Directory Service
#[derive(Clone)]
pub struct DirectoryServiceImpl {
    api: Arc<dyn DirectoryClient>,
}

impl DirectoryServiceImpl {
    pub fn new(api: Arc<dyn DirectoryClient>) -> Self {
        Self { api }
    }
}

#[tonic::async_trait]
impl DirectoryService for DirectoryServiceImpl {
    async fn resolve_grpc_service(
        &self,
        request: Request<ResolveGrpcServiceRequest>,
    ) -> Result<Response<ResolveGrpcServiceResponse>, Status> {
        let service_name = request.into_inner().service_name;

        let endpoint = self
            .api
            .resolve_grpc_service(&service_name)
            .await
            .map_err(|e| map_directory_error(&e))?;

        Ok(Response::new(ResolveGrpcServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn resolve_rest_service(
        &self,
        request: Request<ResolveRestServiceRequest>,
    ) -> Result<Response<ResolveRestServiceResponse>, Status> {
        let module_name = request.into_inner().module_name;

        let endpoint = self
            .api
            .resolve_rest_service(&module_name)
            .await
            .map_err(|e| map_directory_error(&e))?;

        Ok(Response::new(ResolveRestServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn list_instances(
        &self,
        request: Request<ListInstancesRequest>,
    ) -> Result<Response<ListInstancesResponse>, Status> {
        let gear_name = request.into_inner().gear_name;

        let instances = self
            .api
            .list_instances(&gear_name)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let resp = ListInstancesResponse {
            instances: instances
                .into_iter()
                .map(|i| InstanceInfo {
                    gear_name: i.gear,
                    instance_id: i.instance_id,
                    grpc_services: i
                        .grpc_services
                        .into_iter()
                        .map(|svc| cf_system_sdks::directory::GrpcServiceEndpoint {
                            service_name: svc.service_name,
                            endpoint_uri: svc.endpoint.uri,
                        })
                        .collect(),
                    version: i.version.unwrap_or_default(),
                    rest_endpoint_uri: i.rest_endpoint.map(|ep| ep.uri),
                })
                .collect(),
        };

        Ok(Response::new(resp))
    }

    async fn register_instance(
        &self,
        request: Request<RegisterInstanceRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        // Parse endpoints from GrpcServiceEndpoint messages
        let grpc_services = req
            .grpc_services
            .into_iter()
            .map(|svc| cf_system_sdks::directory::GrpcServiceInfo {
                service_name: svc.service_name,
                endpoint: ServiceEndpoint::new(svc.endpoint_uri),
            })
            .collect();

        let info = RegisterInstanceInfo {
            gear: req.gear_name,
            instance_id: req.instance_id,
            grpc_services,
            version: if req.version.is_empty() {
                None
            } else {
                Some(req.version)
            },
            rest_endpoint: req
                .rest_endpoint_uri
                .filter(|uri| !uri.is_empty())
                .map(ServiceEndpoint::new),
        };

        self.api
            .register_instance(info)
            .await
            .map_err(|e| map_directory_error(&e))?;

        Ok(Response::new(()))
    }

    async fn deregister_instance(
        &self,
        request: Request<DeregisterInstanceRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        self.api
            .deregister_instance(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| map_directory_error(&e))?;

        Ok(Response::new(()))
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        self.api
            .send_heartbeat(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| map_directory_error(&e))?;

        Ok(Response::new(()))
    }
}

/// Create a `DirectoryService` server with the given API implementation
pub fn make_directory_service(
    api: Arc<dyn DirectoryClient>,
) -> DirectoryServiceServer<DirectoryServiceImpl> {
    DirectoryServiceServer::new(DirectoryServiceImpl::new(api))
}
