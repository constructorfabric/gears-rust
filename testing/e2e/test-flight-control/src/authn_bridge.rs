use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep_until};
use tonic::codegen::http::{HeaderMap, HeaderValue};
use tonic::metadata::MetadataMap;
use tonic::server::NamedService;
use tonic::{Request, Response, Status};
use toolkit::ClientHub;
use toolkit::context::GearCtx;
use toolkit::contracts::{Gear, GrpcServiceCapability, RegisterGrpcServiceFn};
use toolkit_security::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, PeerAuthenticated,
    PlatformSecurityContext, SecurityContext,
};
use toolkit_trace_context::{TRACEPARENT, X_REQUEST_ID};
use toolkit_transport_grpc::SECCTX_METADATA_KEY;
use tracing::Instrument as _;

use self::wire::authenticate_response::Outcome;
use self::wire::bearer_validation_server::{BearerValidation, BearerValidationServer};
use self::wire::{AuthenticateRequest, AuthenticateResponse, Failure, Identity};

const AUTHORIZATION: &str = "authorization";
const INVOCATION_LIMIT: Duration = Duration::from_secs(5);
const MAX_BEARER_BYTES: usize = 16_384;
const MAX_MESSAGE_BYTES: usize = 131_072;
const MAX_SUBJECT_TYPE_BYTES: usize = 1_024;
const MAX_SCOPES: usize = 64;
const MAX_SCOPE_BYTES: usize = 256;
const MAX_REQUEST_ID_BYTES: usize = 128;
const TRACEPARENT_BYTES: usize = 55;

#[expect(
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::trivially_copy_pass_by_ref,
    reason = "the generated Prost and Tonic source owns these items"
)]
mod wire {
    tonic::include_proto!("test_flight_control.v1");
}

impl std::fmt::Debug for AuthenticateRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticateRequest")
            .finish_non_exhaustive()
    }
}

#[toolkit::gear(name = "test-flight-control-authn", capabilities = [grpc])]
#[derive(Default)]
struct AuthnBridge;

#[tonic::async_trait]
impl Gear for AuthnBridge {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl AuthnBridge {
    fn server<T: BearerValidation>(handler: T) -> BearerValidationServer<T> {
        BearerValidationServer::new(handler)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
    }
}

#[tonic::async_trait]
impl GrpcServiceCapability for AuthnBridge {
    async fn get_grpc_services(&self, ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let server = Self::server(Bridge {
            hub: ctx.client_hub(),
        });
        Ok(vec![RegisterGrpcServiceFn {
            service_name: BearerValidationServer::<Bridge>::NAME,
            register: Box::new(move |routes| {
                routes.add_service(server.clone());
            }),
        }])
    }
}

struct Bridge {
    hub: Arc<ClientHub>,
}

#[tonic::async_trait]
impl BearerValidation for Bridge {
    async fn authenticate(
        &self,
        request: Request<AuthenticateRequest>,
    ) -> Result<Response<AuthenticateResponse>, Status> {
        Self::authenticate_request(request, || {
            self.hub.try_get_local::<DynBearerAuthenticator>()
        })
        .await
        .map(Response::new)
    }
}

impl Bridge {
    async fn authenticate_request(
        request: Request<AuthenticateRequest>,
        lookup: impl FnOnce() -> Option<Arc<DynBearerAuthenticator>>,
    ) -> Result<AuthenticateResponse, Status> {
        let deadline = Instant::now() + INVOCATION_LIMIT;
        let extensions = request.extensions();
        if extensions
            .get::<PlatformSecurityContext>()
            .is_none_or(PlatformSecurityContext::is_outbound_marker)
            || extensions.get::<PeerAuthenticated>().is_none()
        {
            return Err(Status::unauthenticated("platform authentication required"));
        }
        let metadata = request.metadata();
        if metadata.contains_key(AUTHORIZATION) || metadata.contains_key(SECCTX_METADATA_KEY) {
            return Err(Status::invalid_argument("mixed authentication metadata"));
        }

        let bearer_bytes = request.get_ref().bearer_token.len();
        if !(1..=MAX_BEARER_BYTES).contains(&bearer_bytes) {
            return Ok(Failure::InvalidToken.into());
        }
        let Some(authenticator) = lookup() else {
            return Ok(Failure::Unavailable.into());
        };

        let TraceMetadata { trace, request_id } = TraceMetadata::new(metadata);
        let span = tracing::info_span!(
            "fixture_bearer_validation",
            trace_id = tracing::field::Empty,
            parent.trace_id = tracing::field::Empty,
        );
        toolkit_trace_context::set_parent_from_headers(&span, &trace);
        let bearer = request.into_inner().bearer_token;
        let result = toolkit_trace_context::with_request_id(
            request_id,
            async {
                tokio::select! {
                    // The deadline still wins when the authenticator becomes ready at equality.
                    biased;
                    () = sleep_until(deadline) => Err(AuthNError::Unavailable),
                    result = authenticator.authenticate(&bearer) => result,
                }
            }
            .instrument(span),
        )
        .await;

        match result
            .map_err(Failure::from)
            .and_then(|context| Identity::try_from(&context))
        {
            Ok(identity) => Ok(AuthenticateResponse {
                outcome: Some(Outcome::Identity(identity)),
            }),
            Err(failure) => Ok(failure.into()),
        }
    }
}

impl From<Failure> for AuthenticateResponse {
    fn from(failure: Failure) -> Self {
        Self {
            outcome: Some(Outcome::Failure(failure.into())),
        }
    }
}

impl From<AuthNError> for Failure {
    fn from(error: AuthNError) -> Self {
        match error {
            AuthNError::InvalidToken => Self::InvalidToken,
            AuthNError::Unavailable => Self::Unavailable,
            _ => Self::Internal,
        }
    }
}

impl TryFrom<&SecurityContext> for Identity {
    type Error = Failure;

    fn try_from(context: &SecurityContext) -> Result<Self, Self::Error> {
        let subject_type = context.subject_type();
        let scopes = context.token_scopes();
        if subject_type.is_some_and(|value| value.len() > MAX_SUBJECT_TYPE_BYTES)
            || scopes.len() > MAX_SCOPES
            || scopes
                .iter()
                .any(|scope| !(1..=MAX_SCOPE_BYTES).contains(&scope.len()))
        {
            return Err(Failure::Internal);
        }
        Ok(Self {
            subject_id: context.subject_id().to_string(),
            subject_type: subject_type.map(str::to_owned),
            subject_tenant_id: context.subject_tenant_id().to_string(),
            token_scopes: scopes.to_vec(),
        })
    }
}

struct TraceMetadata {
    trace: HeaderMap,
    request_id: Option<String>,
}

impl TraceMetadata {
    fn new(metadata: &MetadataMap) -> Self {
        let mut trace = HeaderMap::new();
        if let Some(value) = Self::traceparent(metadata) {
            trace.insert(TRACEPARENT, value);
        }
        Self {
            trace,
            request_id: Self::request_id(metadata).map(str::to_owned),
        }
    }

    fn traceparent(metadata: &MetadataMap) -> Option<HeaderValue> {
        let value = Self::unique_value(metadata, TRACEPARENT)?;
        if value.len() != TRACEPARENT_BYTES {
            return None;
        }
        toolkit_trace_context::parse_trace_id(value)?;
        HeaderValue::from_str(value).ok()
    }

    fn request_id(metadata: &MetadataMap) -> Option<&str> {
        let value = Self::unique_value(metadata, X_REQUEST_ID)?;
        let valid = (1..=MAX_REQUEST_ID_BYTES).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        valid.then_some(value)
    }

    fn unique_value<'a>(metadata: &'a MetadataMap, key: &'static str) -> Option<&'a str> {
        let values = metadata.get_all(key);
        let mut values = values.iter();
        let value = values.next()?;
        if values.next().is_some() {
            return None;
        }
        value.to_str().ok()
    }
}

#[cfg(test)]
#[path = "authn_bridge_tests.rs"]
mod tests;
