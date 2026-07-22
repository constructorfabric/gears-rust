//! gRPC server implementation for `DirectoryService`
//!
//! This gear provides the gRPC service implementation for Directory Service.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use cf_system_sdks::directory::{
    DeregisterInstanceRequest, DirectoryClient, DirectoryService, DirectoryServiceServer,
    GetOpenApiSpecRequest, GetOpenApiSpecResponse, HeartbeatRequest, InstanceInfo,
    ListInstancesRequest, ListInstancesResponse, RegisterInstanceInfo, RegisterInstanceRequest,
    ResolveGrpcServiceRequest, ResolveGrpcServiceResponse, ResolveRestServiceRequest,
    ResolveRestServiceResponse, ServiceEndpoint,
};

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
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(ResolveGrpcServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn resolve_rest_service(
        &self,
        request: Request<ResolveRestServiceRequest>,
    ) -> Result<Response<ResolveRestServiceResponse>, Status> {
        let gear_name = request.into_inner().gear_name;

        let endpoint = self
            .api
            .resolve_rest_service(&gear_name)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(ResolveRestServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn get_open_api_spec(
        &self,
        request: Request<GetOpenApiSpecRequest>,
    ) -> Result<Response<GetOpenApiSpecResponse>, Status> {
        let gear_name = request.into_inner().gear_name;

        let openapi_spec = self
            .api
            .get_openapi_spec(&gear_name)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(GetOpenApiSpecResponse { openapi_spec }))
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
                    endpoint_uri: i.endpoint.uri,
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
            .map(|svc| (svc.service_name, ServiceEndpoint::new(svc.endpoint_uri)))
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
            rest_endpoint: req.rest_endpoint_uri.map(ServiceEndpoint::new),
            openapi_spec: req.openapi_spec,
        };

        self.api
            .register_instance(info)
            .await
            .map_err(|e| Status::internal(format!("Failed to register instance: {e}")))?;

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
            .map_err(|e| Status::internal(format!("Failed to deregister instance: {e}")))?;

        Ok(Response::new(()))
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        self.api
            .send_heartbeat(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to send heartbeat: {e}")))?;

        Ok(Response::new(()))
    }
}

/// Create a `DirectoryService` server with the given API implementation
pub fn make_directory_service(
    api: Arc<dyn DirectoryClient>,
) -> DirectoryServiceServer<DirectoryServiceImpl> {
    DirectoryServiceServer::new(DirectoryServiceImpl::new(api))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use cf_system_sdks::directory::GrpcServiceEndpoint;
    use toolkit::directory::LocalDirectoryClient;
    use toolkit::runtime::GearManager;
    use uuid::Uuid;

    fn service() -> DirectoryServiceImpl {
        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        DirectoryServiceImpl::new(api)
    }

    #[tokio::test]
    async fn register_then_resolve_rest_and_openapi() {
        let svc = service();

        // Register a gear with a REST endpoint and OpenAPI spec.
        svc.register_instance(Request::new(RegisterInstanceRequest {
            gear_name: "billing".to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            grpc_services: vec![GrpcServiceEndpoint {
                service_name: "billing.Service".to_owned(),
                endpoint_uri: "http://billing:9000".to_owned(),
            }],
            version: "1.0.0".to_owned(),
            rest_endpoint_uri: Some("http://billing:8080".to_owned()),
            openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
        }))
        .await
        .unwrap();

        // Resolve the REST endpoint.
        let rest = svc
            .resolve_rest_service(Request::new(ResolveRestServiceRequest {
                gear_name: "billing".to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(rest.endpoint_uri, "http://billing:8080");

        // Retrieve the OpenAPI spec.
        let spec = svc
            .get_open_api_spec(Request::new(GetOpenApiSpecRequest {
                gear_name: "billing".to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(spec.openapi_spec.contains("openapi"));

        // list_instances carries the REST endpoint back.
        let listed = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: "billing".to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.instances.len(), 1);
        assert_eq!(
            listed.instances[0].rest_endpoint_uri.as_deref(),
            Some("http://billing:8080")
        );
    }

    #[tokio::test]
    async fn resolve_rest_missing_returns_not_found() {
        let svc = service();

        let status = svc
            .resolve_rest_service(Request::new(ResolveRestServiceRequest {
                gear_name: "missing".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);

        let status = svc
            .get_open_api_spec(Request::new(GetOpenApiSpecRequest {
                gear_name: "missing".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    /// Acceptance criteria: a gear registers its REST endpoint + `OpenAPI` spec
    /// and another gear resolves both — end-to-end over gRPC via `DirectoryClient`.
    #[tokio::test]
    async fn grpc_round_trip_register_and_resolve_via_directory_client() {
        use cf_system_sdks::directory::DirectoryGrpcClient;
        use tonic::transport::Server;

        // Directory service backed by an in-memory GearManager.
        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        let grpc_service = make_directory_service(api);

        // Reserve a free port, then let the tonic server bind it.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();

        tokio::spawn(async move {
            Server::builder()
                .add_service(grpc_service)
                .serve(addr)
                .await
                .unwrap();
        });

        // A remote gear talks to the directory purely through DirectoryClient.
        let client: Arc<dyn DirectoryClient> = Arc::new(
            DirectoryGrpcClient::connect(format!("http://{addr}"))
                .await
                .unwrap(),
        );

        // Register a REST endpoint + OpenAPI spec.
        client
            .register_instance(RegisterInstanceInfo {
                gear: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![],
                version: Some("1.0.0".to_owned()),
                rest_endpoint: Some(ServiceEndpoint::http("billing", 8080)),
                openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
            })
            .await
            .unwrap();

        // Resolve both back over the wire.
        let rest = client.resolve_rest_service("billing").await.unwrap();
        assert_eq!(rest.uri, "http://billing:8080");

        let spec = client.get_openapi_spec("billing").await.unwrap();
        assert!(spec.contains("openapi"));
    }
}
