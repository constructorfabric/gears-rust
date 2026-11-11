//! Hand-written gRPC server for `PaymentApi`.
//!
//! Implements the tonic-generated `PaymentApiServer` trait by:
//! 1. Converting inbound `Request<proto::*>` into SDK DTOs via the SDK's
//!    `From`/`Into` bridge.
//! 2. Building a `SecurityContext` from the `authorization` metadata (Bearer
//!    token). With an authenticator ([`PaymentApiGrpcService::with_authenticator`])
//!    the token is validated and the context carries the caller's identity and
//!    tenant, as on the REST path; without one the token is carried unvalidated
//!    and a call without it is anonymous (in-process tests only).
//! 3. Calling into [`crate::domain::service::PaymentDomainService`].
//! 4. Mapping `CanonicalError` → `tonic::Status` (with optional
//!    `ProblemDetails` envelope in trailers).
//!
//! Server codegen is explicitly out-of-scope per PRD ADR-0002 — this is the
//! supported escape hatch for service authors.

use std::pin::Pin;
use std::sync::Arc;

use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_server::{PaymentApi, PaymentApiServer};
use futures_core::Stream;
use tonic::{Code, Request, Response, Status};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::{AuthNError, BearerAuthenticator, DynBearerAuthenticator, SecurityContext};

use crate::domain::service::PaymentDomainService;

/// gRPC service implementation backing the `PaymentApi` contract.
pub struct PaymentApiGrpcService {
    domain: Arc<PaymentDomainService>,
    authenticator: Option<DynBearerAuthenticator>,
}

impl PaymentApiGrpcService {
    /// Wrap the domain service. The bearer token is not validated; use
    /// [`Self::with_authenticator`] wherever the service is reachable by peers.
    #[must_use]
    pub fn new(domain: Arc<PaymentDomainService>) -> Self {
        Self {
            domain,
            authenticator: None,
        }
    }

    /// Validate every call's bearer token with `authenticator`. A call without
    /// one is rejected with `UNAUTHENTICATED`: the contract is tenant-plane.
    ///
    /// grpc-hub checks only the platform-plane internal token, so a gRPC
    /// service has to authenticate the tenant itself.
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: DynBearerAuthenticator) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Convenience constructor returning the tonic Server-trait wrapper.
    #[must_use]
    pub fn into_server(self) -> PaymentApiServer<Self> {
        PaymentApiServer::new(self)
    }

    /// The caller's [`SecurityContext`], or the `Status` to reject the call
    /// with (canonical Problem trailer attached).
    async fn security_context(
        &self,
        metadata: &tonic::metadata::MetadataMap,
    ) -> Result<SecurityContext, Status> {
        let token = match bearer_token(metadata) {
            Ok(token) => token,
            Err(reason) => return Err(unauthenticated(reason)),
        };
        match (&self.authenticator, token) {
            (Some(auth), Some(token)) => auth
                .authenticate(token)
                .await
                .map_err(|e| authn_to_status(&e)),
            (Some(_), None) => Err(unauthenticated("MISSING_BEARER")),
            (None, Some(token)) => SecurityContext::builder()
                .bearer_token(token.to_owned())
                .build()
                .map_err(|_| unauthenticated("INVALID_BEARER")),
            (None, None) => Ok(SecurityContext::anonymous()),
        }
    }
}

#[tonic::async_trait]
impl PaymentApi for PaymentApiGrpcService {
    async fn charge(
        &self,
        request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        let ctx = self.security_context(request.metadata()).await?;
        let proto = request.into_inner();
        // `try_from_proto` surfaces `via_string` parse failures (e.g. malformed
        // UUID strings) as `InvalidArgument` instead of panicking the server.
        let req = api_contracts_sdk::models::ChargeRequest::try_from_proto(&proto)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        match self.domain.charge(&ctx, &req) {
            Ok(resp) => Ok(Response::new(resp.into())),
            Err(e) => Err(canonical_to_status(e)),
        }
    }

    async fn get_invoice(
        &self,
        request: Request<stubs::GetInvoiceRequest>,
    ) -> Result<Response<stubs::Invoice>, Status> {
        let ctx = self.security_context(request.metadata()).await?;
        let invoice_id = request.into_inner().invoice_id;
        match self.domain.get_invoice(&ctx, &invoice_id) {
            Ok(invoice) => Ok(Response::new(invoice.into())),
            Err(e) => Err(canonical_to_status(e)),
        }
    }

    type ListPaymentsStream =
        Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send + 'static>>;

    async fn list_payments(
        &self,
        request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::ListPaymentsStream>, Status> {
        let ctx = self.security_context(request.metadata()).await?;
        let proto = request.into_inner();
        let filter = api_contracts_sdk::models::ListPaymentsFilter::try_from_proto(&proto)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let stream = self.domain.list_payments(&ctx, &filter);

        let mapped = async_stream::try_stream! {
            use futures_util::StreamExt as _;
            let mut s = stream;
            while let Some(item) = s.next().await {
                match item {
                    Ok(summary) => {
                        let proto: stubs::PaymentSummary = summary.into();
                        yield proto;
                    }
                    Err(e) => Err(canonical_to_status(e))?,
                }
            }
        };
        let boxed: Self::ListPaymentsStream = Box::pin(mapped);
        Ok(Response::new(boxed))
    }

    type StreamPaymentsStream =
        Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send + 'static>>;

    /// The fallible-open counterpart of [`Self::list_payments`], and the reason
    /// gRPC needs no special support for that shape: tonic's server trait
    /// method is *already* `async fn(..) -> Result<Response<Self::Stream>,
    /// Status>`. Returning `Err` here sends a `Status` in the response headers,
    /// before any message — which the generated client surfaces as an `Err`
    /// from its own awaited call rather than as the stream's first item.
    ///
    /// So the only difference from `list_payments` below the contract layer is
    /// that the domain's own open (`open_feed`) is `?`-ed rather than folded
    /// into the stream.
    async fn stream_payments(
        &self,
        request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::StreamPaymentsStream>, Status> {
        let ctx = self.security_context(request.metadata()).await?;
        let proto = request.into_inner();
        let filter = api_contracts_sdk::models::ListPaymentsFilter::try_from_proto(&proto)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // The open. A rejected filter becomes a `Status` in the response
        // headers, so no stream is ever created.
        let stream = self
            .domain
            .open_feed(&ctx, &filter)
            .map_err(canonical_to_status)?;

        let mapped = async_stream::try_stream! {
            use futures_util::StreamExt as _;
            let mut s = stream;
            while let Some(item) = s.next().await {
                match item {
                    Ok(summary) => {
                        let proto: stubs::PaymentSummary = summary.into();
                        yield proto;
                    }
                    Err(e) => Err(canonical_to_status(e))?,
                }
            }
        };
        let boxed: Self::StreamPaymentsStream = Box::pin(mapped);
        Ok(Response::new(boxed))
    }
}

/// `Retry-After` hint when the authentication backend is unavailable; the REST
/// middleware uses the same value.
const AUTH_RETRY_AFTER_SECONDS: u64 = 5;

/// The bearer token from the `authorization` metadata: `Ok(None)` when there
/// is none, `Err(reason)` when it is present but malformed. A malformed header
/// is rejected rather than treated as anonymous, which would hide credential
/// bugs. Parsed as the REST middleware does: the scheme is case-insensitive,
/// the token is trimmed and must not contain whitespace.
fn bearer_token(metadata: &tonic::metadata::MetadataMap) -> Result<Option<&str>, &'static str> {
    let Some(raw) = metadata.get("authorization") else {
        return Ok(None);
    };
    let value = raw.to_str().map_err(|_| "INVALID_BEARER")?;
    let (scheme, token) = value
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or("INVALID_BEARER")?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.contains(char::is_whitespace)
    {
        return Err("INVALID_BEARER");
    }
    Ok(Some(token))
}

/// `UNAUTHENTICATED` with a canonical Problem carrying `reason`. The reasons
/// are the REST middleware's: `MISSING_BEARER`, `INVALID_BEARER`,
/// `AUTHN_FAILED`.
fn unauthenticated(reason: &str) -> Status {
    canonical_to_status(
        CanonicalError::unauthenticated()
            .with_reason(reason)
            .create(),
    )
}

/// Maps an authentication failure the way the REST middleware does: a
/// rejected token is `UNAUTHENTICATED`; an unreachable backend is `UNAVAILABLE`
/// with a retry hint; anything else is an infrastructure failure, `INTERNAL`,
/// not the caller's fault. Every status carries a Problem, so the caller's
/// circuit breaker does not count it against this endpoint.
fn authn_to_status(err: &AuthNError) -> Status {
    match err {
        AuthNError::InvalidToken => token_rejected(),
        AuthNError::Unavailable => authn_backend_unavailable(),
        // `AuthNError` is `#[non_exhaustive]`.
        _ => authn_backend_failed(),
    }
}

fn token_rejected() -> Status {
    tracing::warn!("gRPC bearer token rejected: invalid or expired");
    unauthenticated("AUTHN_FAILED")
}

fn authn_backend_unavailable() -> Status {
    tracing::warn!("gRPC bearer token validation: authentication backend unavailable");
    canonical_to_status(
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(AUTH_RETRY_AFTER_SECONDS)
            .create(),
    )
}

fn authn_backend_failed() -> Status {
    tracing::error!("gRPC bearer token validation: unexpected infrastructure failure");
    canonical_to_status(CanonicalError::internal("authentication infrastructure failure").create())
}

/// Map a [`CanonicalError`] onto a [`tonic::Status`] **with** the canonical
/// [`Problem`] envelope attached as a binary trailer
/// (`x-toolkit-problem-bin`, per [`toolkit_transport_grpc::attach_problem`]).
///
/// Direct variant→[`Code`] mapping — not via HTTP status codes — so we
/// don't lose the canonical category through a lossy intermediate
/// representation. Resource info (`resource_type`, `resource_name`) and
/// per-variant context payload travel in the trailer's `Problem.context`,
/// reconstructible by the SDK via `extract_problem`.
fn canonical_to_status(err: CanonicalError) -> Status {
    #[allow(
        clippy::match_same_arms,
        reason = "Explicit `Unknown` arm and the `_` wildcard arm intentionally share a body: the explicit arm documents that the canonical `Unknown` variant maps to `Code::Unknown`, while the wildcard catches future `#[non_exhaustive]` additions. Collapsing them would erase the explicit registration of the known variant."
    )]
    let code = match &err {
        CanonicalError::Cancelled { .. } => Code::Cancelled,
        CanonicalError::Unknown { .. } => Code::Unknown,
        CanonicalError::InvalidArgument { .. } => Code::InvalidArgument,
        CanonicalError::DeadlineExceeded { .. } => Code::DeadlineExceeded,
        CanonicalError::NotFound { .. } => Code::NotFound,
        CanonicalError::AlreadyExists { .. } => Code::AlreadyExists,
        CanonicalError::PermissionDenied { .. } => Code::PermissionDenied,
        CanonicalError::ResourceExhausted { .. } => Code::ResourceExhausted,
        CanonicalError::FailedPrecondition { .. } => Code::FailedPrecondition,
        CanonicalError::Aborted { .. } => Code::Aborted,
        CanonicalError::OutOfRange { .. } => Code::OutOfRange,
        CanonicalError::Unimplemented { .. } => Code::Unimplemented,
        CanonicalError::Internal { .. } => Code::Internal,
        CanonicalError::ServiceUnavailable { .. } => Code::Unavailable,
        CanonicalError::DataLoss { .. } => Code::DataLoss,
        CanonicalError::Unauthenticated { .. } => Code::Unauthenticated,
        // `CanonicalError` is `#[non_exhaustive]`; future variants land
        // here until they get an explicit canonical-category mapping.
        _ => Code::Unknown,
    };
    let detail = err.detail().to_owned();
    let problem = Problem::from(err);
    let mut status = Status::new(code, detail);
    if let Err(attach_err) = toolkit_transport_grpc::attach_problem(status.metadata_mut(), &problem)
    {
        // Attaching the Problem envelope is best-effort: a malformed
        // payload shouldn't suppress the canonical Code. Log so the field
        // shape stays observable in the server logs.
        tracing::warn!(error = %attach_err, "failed to attach Problem trailer");
    }
    status
}
