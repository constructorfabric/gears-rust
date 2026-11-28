//! Bearer validation through the explicitly selected local test host.

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use tonic::codegen::http::HeaderMap;
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, Endpoint};
use toolkit::bootstrap::{OopCompositionContext, OopCompositionError};
use toolkit::contract_support::runtime::config::{CredentialState, InternalTokenProvider};
use toolkit_security::{AuthNError, BearerAuthenticator, DynBearerAuthenticator, SecurityContext};
use toolkit_trace_context::{TRACEPARENT, X_REQUEST_ID};
use toolkit_transport_grpc::internal_auth::attach_internal_token_grpc;
use uuid::Uuid;

use wire::authenticate_response::Outcome;
use wire::bearer_validation_client::BearerValidationClient;
use wire::{AuthenticateRequest, AuthenticateResponse, Failure, Identity};

pub const ENDPOINT_FIELD: &str = "E2E_FLIGHT_CONTROL_ENDPOINT";
const MAX_BEARER_BYTES: usize = 16_384;
const MAX_MESSAGE_BYTES: usize = 131_072;
const CALL_LIMIT: Duration = Duration::from_secs(5);

#[expect(
    clippy::doc_markdown,
    clippy::trivially_copy_pass_by_ref,
    reason = "Prost generates the enum documentation and conversion methods"
)]
mod wire {
    tonic::include_proto!("test_flight_control.v1");
}

impl std::fmt::Debug for AuthenticateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticateRequest")
            .finish_non_exhaustive()
    }
}

fn fixture_endpoint(value: &str) -> Result<Endpoint, OopCompositionError> {
    let invalid = || OopCompositionError::InvalidConfig {
        field: ENDPOINT_FIELD,
    };
    let endpoint = Endpoint::from_shared(value.to_owned()).map_err(|_| invalid())?;
    let uri = endpoint.uri();
    let authority = uri.authority().ok_or_else(invalid)?;
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if uri.scheme_str() != Some("http")
        || uri.path() != "/"
        || uri.query().is_some()
        || value.contains('#')
        || authority.as_str().contains('@')
        || (authority.as_str() != authority.host() && authority.port_u16().is_none())
        || !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    {
        return Err(invalid());
    }
    Ok(endpoint)
}

struct FlightControlBearerAuthenticator {
    endpoint: Endpoint,
    provider: InternalTokenProvider,
    channel: OnceLock<Channel>,
}

pub fn authenticator(
    value: &str,
    context: &OopCompositionContext,
) -> Result<DynBearerAuthenticator, OopCompositionError> {
    let endpoint = fixture_endpoint(value)?;
    let invalid = || OopCompositionError::InvalidConfig {
        field: "oop_http.internal_auth",
    };
    let provider = context
        .internal_token_provider
        .clone()
        .ok_or_else(invalid)?;
    workload_metadata(&provider, &mut MetadataMap::new()).map_err(|_| invalid())?;
    Ok(DynBearerAuthenticator::new(
        FlightControlBearerAuthenticator {
            endpoint,
            provider,
            channel: OnceLock::new(),
        },
    ))
}

fn workload_metadata(
    provider: &InternalTokenProvider,
    metadata: &mut MetadataMap,
) -> Result<(), AuthNError> {
    let CredentialState::Available(token) = provider.current() else {
        return Err(AuthNError::Unavailable);
    };
    attach_internal_token_grpc(metadata, &token).map_err(|_| AuthNError::Unavailable)?;
    let meaningful = metadata
        .get(toolkit_security::constants::INTERNAL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|s| !s.trim().is_empty());
    if !meaningful {
        return Err(AuthNError::Unavailable);
    }
    Ok(())
}

fn attach_correlation(metadata: &mut MetadataMap, headers: &HeaderMap) {
    if let Some(trace) = toolkit_trace_context::get_traceparent(headers)
        && trace.len() == 55
        && toolkit_trace_context::parse_trace_id(trace).is_some()
        && let Ok(value) = trace.parse()
    {
        metadata.insert(TRACEPARENT, value);
    }
    if let Some(id) = toolkit_trace_context::extract_request_id(headers)
        && (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && let Ok(value) = id.parse()
    {
        metadata.insert(X_REQUEST_ID, value);
    }
}

impl FlightControlBearerAuthenticator {
    fn client(&self) -> BearerValidationClient<Channel> {
        let channel = self.channel.get_or_init(|| self.endpoint.connect_lazy());
        BearerValidationClient::new(channel.clone())
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
    }
}

impl BearerAuthenticator for FlightControlBearerAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        let deadline = Instant::now() + CALL_LIMIT;
        if !(1..=MAX_BEARER_BYTES).contains(&token.len()) {
            return Err(AuthNError::InvalidToken);
        }
        let mut request = tonic::Request::new(AuthenticateRequest {
            bearer_token: token.to_owned(),
        });
        workload_metadata(&self.provider, request.metadata_mut())?;
        let mut headers = HeaderMap::new();
        toolkit_trace_context::inject_current_span(&mut headers);
        toolkit::runtime::inject_inbound_traceparent(&mut headers);
        toolkit_trace_context::inject_request_id(&mut headers);
        attach_correlation(request.metadata_mut(), &headers);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AuthNError::Unavailable);
        }
        request.set_timeout(remaining);
        let response = timeout_at(deadline, self.client().authenticate(request))
            .await
            .map_err(|_| AuthNError::Unavailable)?
            .map_err(|_| AuthNError::Unavailable)?;
        let context = decode(response.into_inner(), token);
        if Instant::now() >= deadline {
            return Err(AuthNError::Unavailable);
        }
        context
    }
}

fn malformed() -> AuthNError {
    AuthNError::Other("invalid fixture response".into())
}

fn decode(response: AuthenticateResponse, token: &str) -> Result<SecurityContext, AuthNError> {
    match response.outcome {
        Some(Outcome::Identity(identity)) => identity.context(token),
        Some(Outcome::Failure(code)) => Err(match Failure::try_from(code) {
            Ok(Failure::InvalidToken) => AuthNError::InvalidToken,
            Ok(Failure::Unavailable) => AuthNError::Unavailable,
            _ => malformed(),
        }),
        None => Err(malformed()),
    }
}

impl Identity {
    fn context(self, token: &str) -> Result<SecurityContext, AuthNError> {
        if self.subject_type.as_ref().is_some_and(|s| s.len() > 1024)
            || self.token_scopes.len() > 64
            || self
                .token_scopes
                .iter()
                .any(|s| !(1..=256).contains(&s.len()))
        {
            return Err(malformed());
        }
        let parse = |s: &str| {
            if s.len() != 36 {
                return Err(malformed());
            }
            Uuid::try_parse(s).map_err(|_| malformed())
        };
        let mut context = SecurityContext::builder()
            .subject_id(parse(&self.subject_id)?)
            .subject_tenant_id(parse(&self.subject_tenant_id)?)
            .token_scopes(self.token_scopes)
            .bearer_token(token);
        if let Some(kind) = self.subject_type {
            context = context.subject_type(&kind);
        }
        context.build().map_err(|_| malformed())
    }
}

#[cfg(test)]
#[path = "flight_control_auth_tests.rs"]
mod tests;
