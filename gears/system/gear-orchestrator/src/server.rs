//! gRPC server implementation for `DirectoryService`
//!
//! This gear provides the gRPC service implementation for Directory Service.

use std::sync::Arc;

use secrecy::ExposeSecret;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};
use toolkit_security::{
    DynInternalAuthenticator, InternalAuthNError, InternalAuthenticator, PeerAuthenticated,
};
use toolkit_transport_grpc::extract_internal_token_grpc;

use cf_system_sdks::directory::{
    DeregisterInstanceRequest, DirectoryClient, DirectoryService, DirectoryServiceServer,
    GetOpenApiSpecRequest, GetOpenApiSpecResponse, HeartbeatRequest, InstanceInfo,
    ListAllInstancesRequest, ListAllInstancesResponse, ListInstancesRequest, ListInstancesResponse,
    RegisterInstanceInfo, RegisterInstanceRequest, ResolveGrpcServiceRequest,
    ResolveGrpcServiceResponse, ResolveRestServiceRequest, ResolveRestServiceResponse,
    ServiceEndpoint, ServiceInstanceInfo,
};

/// gRPC service implementation of Directory Service
#[derive(Clone)]
pub struct DirectoryServiceImpl {
    api: Arc<dyn DirectoryClient>,
    /// Platform-plane validator. When `Some`, every RPC requires a valid
    /// `x-toolkit-internal-token` (`cpt-cf-adr-two-plane-auth`); when `None`,
    /// enforcement is disabled (Profile 1 / in-process only).
    authenticator: Option<DynInternalAuthenticator>,
}

impl DirectoryServiceImpl {
    /// Create a `DirectoryService`. When `authenticator` is `Some`, every RPC
    /// validates the platform-plane internal token; when `None`, enforcement
    /// is disabled (Profile 1 / in-process only).
    pub fn with_authenticator(
        api: Arc<dyn DirectoryClient>,
        authenticator: Option<DynInternalAuthenticator>,
    ) -> Self {
        Self { api, authenticator }
    }

    /// Enforce the platform plane on an inbound RPC.
    ///
    /// When an authenticator is configured, the `x-toolkit-internal-token`
    /// metadata must be present and valid; the resolved [`PeerAuthenticated`]
    /// is returned for workload-policy checks (and traced). When no
    /// authenticator is configured, enforcement is skipped.
    async fn authenticate_peer(
        &self,
        meta: &MetadataMap,
    ) -> Result<Option<PeerAuthenticated>, Status> {
        let Some(authenticator) = &self.authenticator else {
            return Ok(None);
        };

        let token = extract_internal_token_grpc(meta)?;
        match authenticator.authenticate(token.expose_secret()).await {
            Ok(identity) => {
                let peer = PeerAuthenticated {
                    name: identity.peer_name().to_owned(),
                };
                tracing::debug!(peer = %peer.name, "platform-plane call authenticated");
                Ok(Some(peer))
            }
            Err(InternalAuthNError::InvalidToken) => {
                Err(Status::unauthenticated("invalid internal token"))
            }
            Err(InternalAuthNError::Unavailable) => {
                Err(Status::unavailable("internal-auth backend unavailable"))
            }
            Err(err) => {
                tracing::warn!(error = %err, "platform-plane authentication failed");
                Err(Status::internal("internal authentication failure"))
            }
        }
    }
}

#[tonic::async_trait]
impl DirectoryService for DirectoryServiceImpl {
    async fn resolve_grpc_service(
        &self,
        request: Request<ResolveGrpcServiceRequest>,
    ) -> Result<Response<ResolveGrpcServiceResponse>, Status> {
        self.authenticate_peer(request.metadata()).await?;
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
        self.authenticate_peer(request.metadata()).await?;
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
        self.authenticate_peer(request.metadata()).await?;
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
        self.authenticate_peer(request.metadata()).await?;
        let gear_name = request.into_inner().gear_name;

        let instances = self
            .api
            .list_instances(&gear_name)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let resp = ListInstancesResponse {
            instances: instances
                .into_iter()
                .map(domain_instance_to_proto)
                .collect(),
        };

        Ok(Response::new(resp))
    }

    async fn list_all_instances(
        &self,
        _request: Request<ListAllInstancesRequest>,
    ) -> Result<Response<ListAllInstancesResponse>, Status> {
        self.authenticate_peer(_request.metadata()).await?;
        let mut instances = self
            .api
            .list_all_instances()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        for inst in &mut instances {
            inst.openapi_spec = None;
        }

        let resp = ListAllInstancesResponse {
            instances: instances
                .into_iter()
                .map(domain_instance_to_proto)
                .collect(),
        };

        Ok(Response::new(resp))
    }

    async fn register_instance(
        &self,
        request: Request<RegisterInstanceRequest>,
    ) -> Result<Response<()>, Status> {
        self.authenticate_peer(request.metadata()).await?;
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
        self.authenticate_peer(request.metadata()).await?;
        let req = request.into_inner();

        self.api
            .deregister_instance(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to deregister instance: {e}")))?;

        Ok(Response::new(()))
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<()>, Status> {
        self.authenticate_peer(request.metadata()).await?;
        let req = request.into_inner();

        self.api
            .send_heartbeat(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to send heartbeat: {e}")))?;

        Ok(Response::new(()))
    }
}

/// Convert a domain [`ServiceInstanceInfo`] into the proto `InstanceInfo`.
fn domain_instance_to_proto(i: ServiceInstanceInfo) -> InstanceInfo {
    InstanceInfo {
        gear_name: i.gear,
        instance_id: i.instance_id,
        endpoint_uri: i.endpoint.uri,
        version: i.version.unwrap_or_default(),
        rest_endpoint_uri: i.rest_endpoint.map(|ep| ep.uri),
        openapi_spec: i.openapi_spec,
    }
}

/// Create a `DirectoryService` server with the given API implementation and
/// optional platform-plane `authenticator`.
///
/// When `authenticator` is `Some`, every RPC requires a valid
/// `x-toolkit-internal-token`; when `None`, enforcement is disabled.
pub fn make_directory_service(
    api: Arc<dyn DirectoryClient>,
    authenticator: Option<DynInternalAuthenticator>,
) -> DirectoryServiceServer<DirectoryServiceImpl> {
    DirectoryServiceServer::new(DirectoryServiceImpl::with_authenticator(api, authenticator))
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
        DirectoryServiceImpl::with_authenticator(api, None)
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
        let grpc_service = make_directory_service(api, None);

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

    /// `list_all_instances` returns every registered gear (across gears) with
    /// its REST endpoint — the edge-gateway discovery path. The full `OpenAPI`
    /// document is intentionally omitted from this snapshot (edge fetches it per
    /// gear via `GetOpenApiSpec`).
    #[tokio::test]
    async fn list_all_instances_returns_all_gears_without_specs() {
        let svc = service();

        for (gear, port) in [("billing", 8080u16), ("catalog", 8081u16)] {
            svc.register_instance(Request::new(RegisterInstanceRequest {
                gear_name: gear.to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![],
                version: "1.0.0".to_owned(),
                rest_endpoint_uri: Some(format!("http://{gear}:{port}")),
                openapi_spec: Some(format!("{{\"openapi\":\"3.1.0\",\"x\":\"{gear}\"}}")),
            }))
            .await
            .unwrap();
        }

        let all = svc
            .list_all_instances(Request::new(ListAllInstancesRequest {}))
            .await
            .unwrap()
            .into_inner()
            .instances;

        assert_eq!(all.len(), 2);
        for inst in &all {
            assert!(inst.rest_endpoint_uri.is_some());
            assert!(
                inst.openapi_spec.is_none(),
                "discovery snapshot must not inline the OpenAPI document"
            );
        }
        let gears: Vec<_> = all.iter().map(|i| i.gear_name.as_str()).collect();
        assert!(gears.contains(&"billing"));
        assert!(gears.contains(&"catalog"));
    }

    /// `cpt-cf-adr-platform-plane-auth` acceptance: with a platform-plane
    /// authenticator installed, the
    /// `DirectoryService` gRPC RPCs reject callers lacking a valid internal
    /// token and accept those that attach the matching shared secret via an
    /// [`InternalAuthInterceptor`] — the full outbound→inbound loop.
    #[tokio::test]
    async fn grpc_enforces_internal_token_end_to_end() {
        use cf_system_sdks::directory::DirectoryGrpcClient;
        use secrecy::SecretString;
        use tonic::transport::Server;
        use toolkit_security::{DynInternalAuthenticator, SharedSecretInternalAuthenticator};
        use toolkit_transport_grpc::InternalAuthInterceptor;

        const SECRET: &str = "dev-internal-token";

        // DirectoryService that enforces the platform plane via a shared secret.
        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        let authenticator = DynInternalAuthenticator::new(SharedSecretInternalAuthenticator::new(
            SecretString::from(SECRET),
            "peer".to_owned(),
        ));
        let grpc_service = make_directory_service(api, Some(authenticator));

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
        let uri = format!("http://{addr}");

        // (1) No credential -> rejected.
        let anon = DirectoryGrpcClient::connect(uri.clone()).await.unwrap();
        assert!(
            anon.list_all_instances().await.is_err(),
            "call without an internal token must be rejected"
        );

        // (2) Wrong credential -> rejected.
        let bad = DirectoryGrpcClient::connect_with_interceptor(
            uri.clone(),
            InternalAuthInterceptor::from_token(SecretString::from("wrong")),
        )
        .await
        .unwrap();
        assert!(
            bad.list_all_instances().await.is_err(),
            "call with an invalid internal token must be rejected"
        );

        // (3) Matching credential -> accepted; register then read back.
        let authed = DirectoryGrpcClient::connect_with_interceptor(
            uri,
            InternalAuthInterceptor::from_token(SecretString::from(SECRET)),
        )
        .await
        .unwrap();
        authed
            .register_instance(RegisterInstanceInfo {
                gear: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![],
                version: Some("1.0.0".to_owned()),
                rest_endpoint: Some(ServiceEndpoint::http("billing", 8080)),
                openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
            })
            .await
            .expect("authenticated register should succeed");
        let all = authed
            .list_all_instances()
            .await
            .expect("authenticated list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].gear, "billing");
    }
}
