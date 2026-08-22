//! gRPC server implementation for `DirectoryService`
//!
//! This gear provides the gRPC service implementation for Directory Service.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use cf_system_sdks::directory::labels::is_valid_label_segment;
use cf_system_sdks::directory::{
    DeregisterInstanceRequest, DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound,
    DirectoryService, DirectoryServiceServer, GetOpenApiSpecRequest, GetOpenApiSpecResponse,
    HeartbeatRequest, InstanceInfo, InstanceState, LabelSelector, ListAllInstancesRequest,
    ListAllInstancesResponse, ListInstancesRequest, ListInstancesResponse, ProtoInstanceState,
    RegisterInstanceInfo, RegisterInstanceRequest, ResolveGrpcServiceRequest,
    ResolveGrpcServiceResponse, ResolveRestServiceRequest, ResolveRestServiceResponse,
    ServiceEndpoint, ServiceInstanceInfo,
};
use std::collections::BTreeMap;

/// Map a lookup failure onto a gRPC status, keeping "not registered" distinct
/// from "the lookup itself failed".
///
/// Clients rely on this distinction: `NotFound` is reconstructed into the
/// `DirectoryNotFound` sentinel and reported as "provider not up yet", which is
/// a routine startup condition rather than an error. Blanket-mapping every
/// failure to `NotFound` would make a genuine internal fault look like a
/// missing registration and leave a consumer waiting forever without a signal.
fn lookup_status(err: &anyhow::Error) -> Status {
    if err.downcast_ref::<DirectoryNotFound>().is_some() {
        Status::not_found(err.to_string())
    } else if err.downcast_ref::<DirectoryInvalidArgument>().is_some() {
        Status::invalid_argument(err.to_string())
    } else {
        Status::internal(err.to_string())
    }
}

/// gRPC service implementation of Directory Service.
///
/// Platform-plane (`x-toolkit-internal-token`) enforcement is applied at the
/// gRPC server boundary by `grpc-hub`'s `InternalAuthGrpcLayer`
/// (`cpt-cf-adr-platform-plane-auth`): the token is validated and, on success, a
/// `PlatformSecurityContext` / `PeerAuthenticated` is placed in the request
/// extensions before the handler runs.
#[derive(Clone)]
pub struct DirectoryServiceImpl {
    api: Arc<dyn DirectoryClient>,
}

impl DirectoryServiceImpl {
    /// Create a `DirectoryService` backed by `api`.
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
        validate_lookup_name("service_name", &service_name)?;

        let endpoint = self
            .api
            .resolve_grpc_service(&service_name)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(ResolveGrpcServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn resolve_rest_service(
        &self,
        request: Request<ResolveRestServiceRequest>,
    ) -> Result<Response<ResolveRestServiceResponse>, Status> {
        let gear_name = request.into_inner().gear_name;
        validate_lookup_name("gear_name", &gear_name)?;

        let endpoint = self
            .api
            .resolve_rest_service(&gear_name)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(ResolveRestServiceResponse {
            endpoint_uri: endpoint.uri,
        }))
    }

    async fn get_open_api_spec(
        &self,
        request: Request<GetOpenApiSpecRequest>,
    ) -> Result<Response<GetOpenApiSpecResponse>, Status> {
        let gear_name = request.into_inner().gear_name;
        validate_lookup_name("gear_name", &gear_name)?;

        let openapi_spec = self
            .api
            .get_openapi_spec(&gear_name)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(GetOpenApiSpecResponse { openapi_spec }))
    }

    async fn list_instances(
        &self,
        request: Request<ListInstancesRequest>,
    ) -> Result<Response<ListInstancesResponse>, Status> {
        let req = request.into_inner();
        let gear_name = req.gear_name;
        validate_lookup_name("gear_name", &gear_name)?;
        let selector = LabelSelector::from_match_labels(
            req.match_labels.into_iter().collect::<BTreeMap<_, _>>(),
        );
        // Validate the caller-supplied selector against the same rules stored
        // labels obey: a malformed selector can never match a valid label, and
        // echoing it back is unsafe for the same reason register-time labels
        // are screened.
        cf_system_sdks::directory::validate_selector(&selector)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // Enumerate once and filter by the selector (an empty selector matches
        // all). The response is always spec-free: only the `openapi_spec_hash`
        // rides along, and the full document is fetched out-of-band via
        // `GetOpenApiSpec`. This bounds every multi-instance response regardless
        // of spec size, matching `resolve_by_labels` / `list_all_instances`
        // (ADR-0009); the hash still lets a consumer detect a spec change.
        let mut instances = self
            .api
            .list_instances(&gear_name)
            .await
            .map_err(|e| lookup_status(&e))?;
        instances.retain(|inst| selector.matches(&inst.labels));
        for inst in &mut instances {
            inst.openapi_spec = None;
        }

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
        let instances = self
            .api
            .list_all_instances()
            .await
            .map_err(|e| lookup_status(&e))?;

        let resp = ListAllInstancesResponse {
            instances: instances
                .into_iter()
                // `openapi_spec` and `labels` are both omitted from the broad
                // cross-gear snapshot; `without_labels` is the shared transform
                // (see the `list_all_instances` trait doc). The spec is dropped
                // here too — it is never inlined into this snapshot.
                .map(|inst| domain_instance_to_proto(inst.without_labels()))
                .map(|mut proto| {
                    proto.openapi_spec = None;
                    proto
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

        validate_identity(&req.gear_name, &req.instance_id)?;
        validate_labels(&req.labels)?;
        for (idx, svc) in req.grpc_services.iter().enumerate() {
            // Identify the service by index, never by interpolating the
            // caller-controlled `service_name` into the (reflected) status
            // context. The name is validated as a label segment first so an
            // invalid one is rejected before storage or discovery.
            if !is_valid_label_segment(&svc.service_name) {
                return Err(Status::invalid_argument(format!(
                    "grpc service #{idx}: service name contains invalid characters"
                )));
            }
            validate_endpoint_uri(&format!("grpc service #{idx}"), &svc.endpoint_uri)?;
        }
        if let Some(uri) = &req.rest_endpoint_uri {
            validate_endpoint_uri("rest endpoint", uri)?;
        }
        if let Some(spec) = &req.openapi_spec {
            validate_openapi_spec(spec)?;
        }

        // Parse endpoints from GrpcServiceEndpoint messages
        let grpc_services = req
            .grpc_services
            .into_iter()
            .map(|svc| (svc.service_name, ServiceEndpoint::new(svc.endpoint_uri)))
            .collect();

        let mut info = RegisterInstanceInfo::new(req.gear_name, req.instance_id)
            .with_grpc_services(grpc_services)
            .with_labels(req.labels.into_iter().collect());
        if !req.version.is_empty() {
            info = info.with_version(req.version);
        }
        if let Some(uri) = req.rest_endpoint_uri {
            info = info.with_rest_endpoint(ServiceEndpoint::new(uri));
        }
        if let Some(spec) = req.openapi_spec {
            info = info.with_openapi_spec(spec);
        }

        self.api
            .register_instance(info)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(()))
    }

    async fn deregister_instance(
        &self,
        request: Request<DeregisterInstanceRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        validate_identity(&req.gear_name, &req.instance_id)?;
        self.api
            .deregister_instance(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(()))
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        validate_identity(&req.gear_name, &req.instance_id)?;
        self.api
            .send_heartbeat(&req.gear_name, &req.instance_id)
            .await
            .map_err(|e| lookup_status(&e))?;

        Ok(Response::new(()))
    }
}

/// Convert a domain [`ServiceInstanceInfo`] into the proto `InstanceInfo`.
fn domain_instance_to_proto(i: ServiceInstanceInfo) -> InstanceInfo {
    InstanceInfo {
        gear_name: i.gear,
        instance_id: i.instance_id,
        endpoint_uri: i.endpoint.map(|ep| ep.uri).unwrap_or_default(),
        version: i.version.unwrap_or_default(),
        rest_endpoint_uri: i.rest_endpoint.map(|ep| ep.uri),
        openapi_spec: i.openapi_spec,
        openapi_spec_hash: i.openapi_spec_hash,
        labels: i.labels.into_iter().collect(),
        state: domain_state_to_proto(i.state) as i32,
    }
}

/// Map the domain [`InstanceState`] onto the proto `InstanceState` enum so a
/// label-targeted caller can apply its own health policy from one resolve.
fn domain_state_to_proto(state: InstanceState) -> ProtoInstanceState {
    match state {
        InstanceState::Registered => ProtoInstanceState::Registered,
        InstanceState::Ready => ProtoInstanceState::Ready,
        InstanceState::Healthy => ProtoInstanceState::Healthy,
        InstanceState::Quarantined => ProtoInstanceState::Quarantined,
        InstanceState::Draining => ProtoInstanceState::Draining,
        InstanceState::Unknown => ProtoInstanceState::Unspecified,
    }
}

/// Upper bound on a single registrant-supplied endpoint URI. Endpoint URIs are
/// echoed verbatim to every `resolve_*` / `list_*` consumer, so they are
/// bounded and screened for control characters at the trust boundary.
const MAX_ENDPOINT_URI_LEN: usize = 2048;

/// Upper bound on a registrant-supplied `OpenAPI` document. Like endpoint URIs and
/// labels, the spec is echoed to consumers - returned verbatim by
/// `GetOpenApiSpec`, inlined into `list_instances` (when requested), and turned
/// into a public route table by the edge - so it is bounded at the trust
/// boundary rather than stored unbounded. This is a size guard only; structural
/// `OpenAPI`/JSON validation is tracked separately by
/// `cpt-cf-binding-fr-openapi-validation`. The cap sits well under the default
/// 4 MiB gRPC message size so several specs can still be inlined in one
/// `list_instances` response.
const MAX_OPENAPI_SPEC_LEN: usize = 1024 * 1024;

/// Schemes accepted for a registered endpoint URI.
///
/// Both endpoint consumers speak HTTP transports only: gRPC service endpoints
/// are dialed via tonic's `Endpoint::from_shared` (default TCP connector) and
/// REST endpoints are reverse-proxied by the `api-gateway` forwarder, which
/// parses them as an `http::Uri` and forwards over `http`/`https`. No consumer
/// connects over a Unix domain socket, so `unix` (and every other scheme) is
/// rejected here rather than stored and handed back to a consumer that cannot
/// dial it.
const SUPPORTED_ENDPOINT_SCHEMES: [&str; 2] = ["http", "https"];

/// Reject an endpoint URI that is empty, over [`MAX_ENDPOINT_URI_LEN`], carries
/// ASCII control/whitespace characters, is syntactically malformed, is missing
/// its scheme or authority, or uses a scheme no endpoint consumer can dial (see
/// [`SUPPORTED_ENDPOINT_SCHEMES`]).
///
/// The URI originates from an untrusted registrant and is later handed back to
/// every discovery consumer, so screening it here keeps a malformed,
/// undialable, or injection-style value from being stored and propagated. The
/// URI is parsed with the same [`http::Uri`] parser the gRPC transport and REST
/// proxy use, so a value accepted here is one those consumers can also parse.
fn validate_endpoint_uri(context: &str, uri: &str) -> Result<(), Status> {
    if uri.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{context}: empty endpoint URI"
        )));
    }
    if uri.len() > MAX_ENDPOINT_URI_LEN {
        return Err(Status::invalid_argument(format!(
            "{context}: endpoint URI too long: {} (max {MAX_ENDPOINT_URI_LEN})",
            uri.len()
        )));
    }
    if uri
        .chars()
        .any(|c| c.is_ascii_control() || c.is_whitespace())
    {
        return Err(Status::invalid_argument(format!(
            "{context}: endpoint URI contains control or whitespace characters"
        )));
    }
    let parsed = uri.parse::<http::Uri>().map_err(|err| {
        Status::invalid_argument(format!("{context}: malformed endpoint URI: {err}"))
    })?;
    let scheme = parsed.scheme_str().ok_or_else(|| {
        Status::invalid_argument(format!("{context}: endpoint URI is missing a scheme"))
    })?;
    if !SUPPORTED_ENDPOINT_SCHEMES.contains(&scheme) {
        return Err(Status::invalid_argument(format!(
            "{context}: unsupported endpoint scheme '{scheme}' (expected one of \
             {SUPPORTED_ENDPOINT_SCHEMES:?})"
        )));
    }
    let authority = parsed.authority().ok_or_else(|| {
        Status::invalid_argument(format!("{context}: endpoint URI missing authority (host)"))
    })?;
    if authority.as_str().contains('@') {
        return Err(Status::invalid_argument(format!(
            "{context}: endpoint URI must not contain userinfo ('@')"
        )));
    }
    if parsed.host().is_none_or(str::is_empty) {
        return Err(Status::invalid_argument(format!(
            "{context}: endpoint URI missing host"
        )));
    }
    Ok(())
}

/// Reject a registrant-supplied `OpenAPI` document over [`MAX_OPENAPI_SPEC_LEN`].
///
/// The spec originates from an untrusted registrant and is later echoed to
/// discovery consumers - returned verbatim by `GetOpenApiSpec`, inlined into
/// `list_instances`, and rendered into the edge's public route table - so an
/// unbounded value would let a single registrant inflate every such response.
/// This bounds size only; structural `OpenAPI`/JSON validation is the separate
/// `cpt-cf-binding-fr-openapi-validation`.
fn validate_openapi_spec(spec: &str) -> Result<(), Status> {
    if spec.len() > MAX_OPENAPI_SPEC_LEN {
        return Err(Status::invalid_argument(format!(
            "openapi spec too long: {} bytes (max {MAX_OPENAPI_SPEC_LEN})",
            spec.len()
        )));
    }
    Ok(())
}

/// Reject a registrant whose labels violate the directory's label rules.
///
/// The rules themselves live in the directory SDK
/// ([`cf_system_sdks::directory::labels`]) so every boundary a label can enter
/// through — this gRPC front-end, the in-process store, and a gear's own
/// start-up — enforces them identically. Here we only adapt the shared
/// [`LabelValidationError`] onto a gRPC `Status`. The registrant is remote and
/// untrusted, so this runs before the map is stored and echoed on every
/// `list_instances` / `resolve_by_labels` response; the error message never
/// interpolates the offending (caller-controlled) key/value.
fn validate_labels(labels: &std::collections::HashMap<String, String>) -> Result<(), Status> {
    cf_system_sdks::directory::labels::validate_label_pairs(
        labels.len(),
        labels.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )
    .map_err(|e| Status::invalid_argument(e.to_string()))
}

/// Reject a lookup key (gear or service name) that is blank or violates the
/// segment charset.
///
/// This is the **same** screen `register_instance` applies on write, run here on
/// the read RPCs so both ends of the contract agree on what a valid name is: a
/// malformed name is `InvalidArgument` on lookup and register alike, while a
/// valid-but-unknown name stays a clean not-found — never conflated with a
/// caller bug. `context` is a static field label, so the offending
/// (caller-controlled) value is never interpolated into the message.
fn validate_lookup_name(context: &str, name: &str) -> Result<(), Status> {
    if name.trim().is_empty() {
        return Err(Status::invalid_argument(format!(
            "{context} must not be empty"
        )));
    }
    if !is_valid_label_segment(name) {
        return Err(Status::invalid_argument(format!(
            "{context} contains invalid characters"
        )));
    }
    Ok(())
}

/// Reject a registrant whose `(gear_name, instance_id)` identity is malformed.
///
/// A blank/malformed gear name or an `instance_id` that is not a UUID is a
/// client error, not a server fault: validating it at the trust boundary returns
/// `InvalidArgument` (so the caller's presence loop stops retrying a
/// permanently-invalid request) instead of surfacing as a bare
/// `anyhow!("Invalid instance_id ...")` mislabelled `Internal` from deeper in
/// the store. The gear-name charset screen ([`validate_lookup_name`]) also
/// rejects control / whitespace / reflected characters before storage, since the
/// name is echoed on every discovery response.
fn validate_identity(gear_name: &str, instance_id: &str) -> Result<(), Status> {
    validate_lookup_name("gear_name", gear_name)?;
    if uuid::Uuid::parse_str(instance_id).is_err() {
        return Err(Status::invalid_argument("instance_id must be a valid UUID"));
    }
    Ok(())
}

/// Create a `DirectoryService` server backed by `api`.
///
/// Platform-plane enforcement is applied at the gRPC server boundary by
/// `grpc-hub`'s `InternalAuthGrpcLayer`.
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
    use cf_system_sdks::directory::labels::{
        MAX_LABEL_KEY_LEN, MAX_LABEL_VALUE_LEN, MAX_LABELS, is_valid_label_segment,
    };
    use toolkit::directory::LocalDirectoryClient;
    use toolkit::runtime::GearManager;
    use uuid::Uuid;

    fn service() -> DirectoryServiceImpl {
        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        DirectoryServiceImpl::new(api)
    }

    #[tokio::test]
    async fn register_rejects_oversized_labels() {
        let svc = service();

        // Too many entries.
        let many: std::collections::HashMap<String, String> = (0..=MAX_LABELS)
            .map(|i| (format!("k{i}"), "v".to_owned()))
            .collect();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                labels: many,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);

        // Over-long value.
        let long: std::collections::HashMap<String, String> =
            [("shard".to_owned(), "x".repeat(MAX_LABEL_VALUE_LEN + 1))].into();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                labels: long,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);

        // A request exactly at the limits still registers.
        let at_limit: std::collections::HashMap<String, String> = [(
            "k".repeat(MAX_LABEL_KEY_LEN),
            "v".repeat(MAX_LABEL_VALUE_LEN),
        )]
        .into();
        svc.register_instance(Request::new(RegisterInstanceRequest {
            gear_name: "billing".to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            labels: at_limit,
            ..Default::default()
        }))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn register_rejects_malformed_endpoint_uri() {
        let svc = service();

        // Control/whitespace character in a gRPC endpoint URI.
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![GrpcServiceEndpoint {
                    service_name: "billing.Service".to_owned(),
                    endpoint_uri: "http://billing:9000\n".to_owned(),
                }],
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);

        // Empty REST endpoint URI.
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                rest_endpoint_uri: Some(String::new()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_endpoint_uri_accepts_supported_schemes() {
        // The transports every endpoint consumer can actually dial.
        validate_endpoint_uri("grpc", "http://billing:9000").unwrap();
        validate_endpoint_uri("rest", "https://billing.svc:8443").unwrap();
        // Path/query are tolerated (consumers keep only scheme + authority).
        validate_endpoint_uri("rest", "https://billing.svc/ignored?x=1").unwrap();
    }

    #[test]
    fn validate_endpoint_uri_rejects_malformed_syntax() {
        // Not parseable as a URI at all.
        let err = validate_endpoint_uri("grpc", "http://[::1").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_endpoint_uri_rejects_missing_components() {
        // No scheme (bare origin-form path).
        let err = validate_endpoint_uri("grpc", "/billing/v1").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        // Supported scheme but no authority (host).
        let err = validate_endpoint_uri("rest", "http:///path").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_endpoint_uri_rejects_userinfo() {
        // A `user@host` authority parses, but every consumer dials the host
        // after the `@` while the stored value reads as the userinfo prefix, so
        // a trusted-looking `http://billing@evil.com` would redirect traffic.
        for uri in [
            "http://billing@evil.com:9000",
            "https://billing@evil.com",
            "http://user:pass@evil.com",
        ] {
            let err = validate_endpoint_uri("grpc", uri).unwrap_err();
            assert_eq!(
                err.code(),
                tonic::Code::InvalidArgument,
                "expected {uri} to be rejected"
            );
        }
    }

    #[test]
    fn validate_endpoint_uri_rejects_hostless_authority() {
        // Authority present but no host (`http://:8080`) is undialable.
        let err = validate_endpoint_uri("rest", "http://:8080").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_endpoint_uri_rejects_unsupported_schemes() {
        // No endpoint consumer speaks these transports.
        for uri in [
            "ftp://billing:21",
            "ws://billing:9000",
            "file:///etc/passwd",
        ] {
            let err = validate_endpoint_uri("grpc", uri).unwrap_err();
            assert_eq!(
                err.code(),
                tonic::Code::InvalidArgument,
                "expected {uri} to be rejected"
            );
        }
    }

    #[test]
    fn validate_endpoint_uri_rejects_unix_until_consumers_support_uds() {
        // Neither the tonic gRPC channel (default TCP connector) nor the
        // api-gateway forwarder can dial a Unix domain socket, so a `unix://`
        // endpoint is rejected at the trust boundary. Flip this to an accept
        // (and extend SUPPORTED_ENDPOINT_SCHEMES) once every consumer supports
        // UDS.
        let err = validate_endpoint_uri("rest", "unix:///run/billing.sock").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn register_rejects_unsupported_endpoint_scheme() {
        let svc = service();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                rest_endpoint_uri: Some("ftp://billing:21".to_owned()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn validate_openapi_spec_bounds_length() {
        // At the cap passes; one byte over is rejected. The spec is echoed to
        // every consumer, so an oversized value is refused at registration.
        validate_openapi_spec(&"x".repeat(MAX_OPENAPI_SPEC_LEN)).unwrap();
        let err = validate_openapi_spec(&"x".repeat(MAX_OPENAPI_SPEC_LEN + 1)).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn register_rejects_oversized_openapi_spec() {
        let svc = service();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                rest_endpoint_uri: Some("http://billing:8080".to_owned()),
                openapi_spec: Some("x".repeat(MAX_OPENAPI_SPEC_LEN + 1)),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_all_instances_omits_labels() {
        let svc = service();
        svc.register_instance(Request::new(RegisterInstanceRequest {
            gear_name: "billing".to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            rest_endpoint_uri: Some("http://billing:8080".to_owned()),
            labels: [("shard".to_owned(), "a".to_owned())].into(),
            ..Default::default()
        }))
        .await
        .unwrap();

        let all = svc
            .list_all_instances(Request::new(ListAllInstancesRequest {}))
            .await
            .unwrap()
            .into_inner()
            .instances;
        assert_eq!(all.len(), 1);
        assert!(
            all[0].labels.is_empty(),
            "cross-gear list_all snapshot must not carry labels"
        );
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
            ..Default::default()
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
                ..Default::default()
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
            .register_instance(
                RegisterInstanceInfo::new("billing", Uuid::new_v4().to_string())
                    .with_version("1.0.0")
                    .with_rest_endpoint(ServiceEndpoint::http("billing", 8080))
                    .with_openapi_spec("{\"openapi\":\"3.1.0\"}"),
            )
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
                ..Default::default()
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

    /// `cpt-cf-adr-platform-plane-auth` acceptance: with the platform-plane
    /// [`InternalAuthGrpcLayer`] on the server (as `grpc-hub` wires it), the
    /// `DirectoryService` gRPC RPCs reject callers lacking a valid internal
    /// token and accept those attaching the matching shared secret via an
    /// [`InternalAuthInterceptor`] — the full outbound→inbound loop.
    #[tokio::test]
    async fn grpc_enforces_internal_token_end_to_end() {
        use cf_system_sdks::directory::DirectoryGrpcClient;
        use secrecy::SecretString;
        use tonic::transport::Server;
        use toolkit_security::{DynInternalAuthenticator, SharedSecretInternalAuthenticator};
        use toolkit_transport_grpc::{InternalAuthGrpcLayer, InternalAuthInterceptor};

        const SECRET: &str = "dev-internal-token";

        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        let grpc_service = make_directory_service(api);

        // Required mode: an absent token is rejected.
        let authenticator = DynInternalAuthenticator::new(SharedSecretInternalAuthenticator::new(
            SecretString::from(SECRET),
            "peer".to_owned(),
        ));
        let auth_layer = InternalAuthGrpcLayer::new(Some(authenticator));

        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        // Abort the server on test exit so it does not outlive the test with the
        // ephemeral port still bound.
        let server = tokio::spawn(async move {
            Server::builder()
                .layer(auth_layer)
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
            .register_instance(
                RegisterInstanceInfo::new("billing", Uuid::new_v4().to_string())
                    .with_version("1.0.0")
                    .with_rest_endpoint(ServiceEndpoint::http("billing", 8080))
                    .with_openapi_spec("{\"openapi\":\"3.1.0\"}"),
            )
            .await
            .expect("authenticated register should succeed");
        let all = authed
            .list_all_instances()
            .await
            .expect("authenticated list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].gear, "billing");

        server.abort();
    }

    #[test]
    fn domain_state_maps_to_proto_over_all_variants() {
        // Each domain state maps to its proto counterpart; Unknown collapses to
        // the UNSPECIFIED wire sentinel. Guards against a silent all-Registered
        // or swapped-variant regression.
        for (domain, proto) in [
            (InstanceState::Registered, ProtoInstanceState::Registered),
            (InstanceState::Ready, ProtoInstanceState::Ready),
            (InstanceState::Healthy, ProtoInstanceState::Healthy),
            (InstanceState::Quarantined, ProtoInstanceState::Quarantined),
            (InstanceState::Draining, ProtoInstanceState::Draining),
            (InstanceState::Unknown, ProtoInstanceState::Unspecified),
        ] {
            assert_eq!(domain_state_to_proto(domain), proto);
        }
    }

    /// The projected `state` field is real: a freshly registered instance reads
    /// `Registered`, and after a heartbeat it reads `Healthy` in the
    /// `list_instances` response.
    #[tokio::test]
    async fn list_instances_projects_live_state() {
        let svc = service();
        let instance_id = Uuid::new_v4().to_string();

        svc.register_instance(Request::new(RegisterInstanceRequest {
            gear_name: "billing".to_owned(),
            instance_id: instance_id.clone(),
            rest_endpoint_uri: Some("http://billing:8080".to_owned()),
            ..Default::default()
        }))
        .await
        .unwrap();

        let state_of = |svc: &DirectoryServiceImpl| {
            let svc = svc.clone();
            async move {
                svc.list_instances(Request::new(ListInstancesRequest {
                    gear_name: "billing".to_owned(),
                    ..Default::default()
                }))
                .await
                .unwrap()
                .into_inner()
                .instances[0]
                    .state
            }
        };

        assert_eq!(
            state_of(&svc).await,
            ProtoInstanceState::Registered as i32,
            "a freshly registered instance is not yet serving"
        );

        svc.heartbeat(Request::new(HeartbeatRequest {
            gear_name: "billing".to_owned(),
            instance_id,
        }))
        .await
        .unwrap();

        assert_eq!(
            state_of(&svc).await,
            ProtoInstanceState::Healthy as i32,
            "a heartbeat transitions the instance to Healthy"
        );
    }

    #[tokio::test]
    async fn register_rejects_malformed_identity() {
        let svc = service();

        // Non-UUID instance_id.
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: "not-a-uuid".to_owned(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);

        // Empty gear name.
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: String::new(),
                instance_id: Uuid::new_v4().to_string(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn register_rejects_labels_with_disallowed_charset() {
        let svc = service();
        // A control character in a label value must be rejected (and never
        // reflected into the returned Status).
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                labels: [("shard".to_owned(), "1\r\n2".to_owned())].into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            !err.message().contains('\n'),
            "raw label value must not be echoed into the status message"
        );
    }

    #[tokio::test]
    async fn register_rejects_gear_name_with_disallowed_charset() {
        let svc = service();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "bad\r\nname".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            !err.message().contains('\n'),
            "raw gear name must not be echoed into the status message"
        );
    }

    #[tokio::test]
    async fn register_rejects_grpc_service_name_with_disallowed_charset() {
        let svc = service();
        let err = svc
            .register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "billing".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![GrpcServiceEndpoint {
                    service_name: "bad\r\nservice".to_owned(),
                    endpoint_uri: "http://127.0.0.1:50051".to_owned(),
                }],
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            !err.message().contains('\n'),
            "raw service name must not be echoed into the status message"
        );
    }

    #[tokio::test]
    async fn lookup_rpcs_reject_malformed_name_with_invalid_argument() {
        let svc = service();

        // Both ends of the contract agree: a malformed name is InvalidArgument
        // on the read RPCs, not a silent not-found the caller reads as "not
        // started yet". The raw (caller-controlled) name is never reflected.
        let rest = svc
            .resolve_rest_service(Request::new(ResolveRestServiceRequest {
                gear_name: "bad\r\nname".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(rest.code(), tonic::Code::InvalidArgument);
        assert!(!rest.message().contains('\n'));

        let grpc = svc
            .resolve_grpc_service(Request::new(ResolveGrpcServiceRequest {
                service_name: "bad\r\nservice".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(grpc.code(), tonic::Code::InvalidArgument);

        let spec = svc
            .get_open_api_spec(Request::new(GetOpenApiSpecRequest {
                gear_name: "bad name".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(spec.code(), tonic::Code::InvalidArgument);

        let list = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: String::new(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(list.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn lookup_rpcs_report_valid_unknown_name_as_not_found() {
        let svc = service();

        // A valid-but-unregistered name stays a clean not-found — distinct from
        // the InvalidArgument a malformed name earns above.
        let rest = svc
            .resolve_rest_service(Request::new(ResolveRestServiceRequest {
                gear_name: "billing".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(rest.code(), tonic::Code::NotFound);

        // A valid unknown name still lists cleanly (empty), not an error.
        let list = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: "billing".to_owned(),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(list.into_inner().instances.is_empty());
    }

    #[test]
    // The `naïve` literal is intentional: it exercises rejection of a non-ASCII
    // segment, which is exactly what this charset test asserts.
    #[allow(clippy::non_ascii_literal)]
    fn label_segment_charset_rules() {
        for ok in ["shard", "1", "role-a", "a.b_c", "v1.2.3"] {
            assert!(is_valid_label_segment(ok), "{ok} should be valid");
        }
        for bad in ["", "-lead", "trail_", ".dot", "a=b", "a b", "a\tb", "naïve"] {
            assert!(!is_valid_label_segment(bad), "{bad} should be invalid");
        }
    }

    /// The `list_instances` label-selector branch: a non-empty `match_labels`
    /// returns only matching instances, spec-free; a selector matching nothing
    /// returns an empty list.
    #[tokio::test]
    async fn list_instances_with_match_labels_filters_and_strips_spec() {
        let svc = service();

        // Two instances of the same gear on different shards, both with specs.
        for shard in ["1", "2"] {
            svc.register_instance(Request::new(RegisterInstanceRequest {
                gear_name: "ingest".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                version: "1.0.0".to_owned(),
                rest_endpoint_uri: Some(format!("http://ingest-{shard}:8080")),
                openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
                labels: [("shard".to_owned(), shard.to_owned())].into(),
                ..Default::default()
            }))
            .await
            .unwrap();
        }

        // A selector pins exactly one shard, returned spec-free.
        let matched = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: "ingest".to_owned(),
                match_labels: [("shard".to_owned(), "1".to_owned())].into(),
            }))
            .await
            .unwrap()
            .into_inner()
            .instances;
        assert_eq!(
            matched.len(),
            1,
            "selector must return only the matching shard"
        );
        assert_eq!(
            matched[0].labels.get("shard").map(String::as_str),
            Some("1")
        );
        assert!(
            matched[0].openapi_spec.is_none(),
            "label-resolve path must strip the OpenAPI document"
        );

        // A selector matching nothing returns an empty list (not an error).
        let none = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: "ingest".to_owned(),
                match_labels: [("shard".to_owned(), "99".to_owned())].into(),
            }))
            .await
            .unwrap()
            .into_inner()
            .instances;
        assert!(none.is_empty(), "no instance carries shard=99");

        // Every `list_instances` response is spec-free (empty selector matches
        // all): the document is never inlined, but the hash still rides along so
        // a consumer can detect a spec change and fetch it via GetOpenApiSpec.
        let all_spec_free = svc
            .list_instances(Request::new(ListInstancesRequest {
                gear_name: "ingest".to_owned(),
                match_labels: std::collections::HashMap::new(),
            }))
            .await
            .unwrap()
            .into_inner()
            .instances;
        assert_eq!(all_spec_free.len(), 2, "empty selector matches every shard");
        assert!(
            all_spec_free.iter().all(|i| i.openapi_spec.is_none()),
            "list_instances must never inline the OpenAPI document"
        );
        assert!(
            all_spec_free.iter().all(|i| i.openapi_spec_hash.is_some()),
            "list_instances must still carry the spec hash"
        );
    }

    /// End-to-end over gRPC via `DirectoryClient`: `resolve_by_labels` returns
    /// only the instances whose labels satisfy the selector, and each carries
    /// no inline `OpenAPI` document.
    #[tokio::test]
    async fn grpc_resolve_by_labels_returns_only_matching_spec_free() {
        use cf_system_sdks::directory::{DirectoryGrpcClient, LabelSelector};
        use tonic::transport::Server;

        let manager = Arc::new(GearManager::new());
        let api: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(manager));
        let grpc_service = make_directory_service(api);

        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(grpc_service)
                .serve(addr)
                .await
                .unwrap();
        });

        let client: Arc<dyn DirectoryClient> = Arc::new(
            DirectoryGrpcClient::connect(format!("http://{addr}"))
                .await
                .unwrap(),
        );

        // Two shards of the same gear; each publishes an OpenAPI document.
        for shard in ["1", "2"] {
            client
                .register_instance(
                    RegisterInstanceInfo::new("ingest", Uuid::new_v4().to_string())
                        .with_rest_endpoint(ServiceEndpoint::http(&format!("ingest-{shard}"), 8080))
                        .with_openapi_spec("{\"openapi\":\"3.1.0\"}")
                        .with_labels([("shard".to_owned(), shard.to_owned())].into()),
                )
                .await
                .unwrap();
        }

        // Selector pins shard 1: exactly one match, spec-free over the wire.
        let matched = client
            .resolve_by_labels("ingest", &LabelSelector::new().with("shard", "1"))
            .await
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(
            matched[0].labels.get("shard").map(String::as_str),
            Some("1")
        );
        assert!(
            matched[0].openapi_spec.is_none(),
            "resolve_by_labels must not carry the OpenAPI document"
        );

        // A selector matching nothing yields an empty set (not an error).
        let none = client
            .resolve_by_labels("ingest", &LabelSelector::new().with("shard", "99"))
            .await
            .unwrap();
        assert!(none.is_empty());

        let all = client
            .resolve_by_labels("ingest", &LabelSelector::new())
            .await
            .unwrap();
        assert_eq!(all.len(), 2, "empty selector matches every shard");
        assert!(
            all.iter().all(|i| i.openapi_spec.is_none()),
            "empty-selector resolve_by_labels must not carry the OpenAPI document"
        );

        server.abort();
    }
}
