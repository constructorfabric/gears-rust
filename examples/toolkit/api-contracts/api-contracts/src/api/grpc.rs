//! Hand-written gRPC server for `PaymentApi`.
//!
//! Implements the tonic-generated `PaymentApiServer` trait by:
//! 1. Converting inbound `Request<proto::*>` into SDK DTOs via the SDK's
//!    `From`/`Into` bridge.
//! 2. Reconstructing a `SecurityContext` from the `authorization` metadata
//!    header (Bearer token), or falling back to anonymous.
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
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::SecurityContext;
use tonic::{Code, Request, Response, Status};

use crate::domain::service::PaymentDomainService;

/// gRPC service implementation backing the `PaymentApi` contract.
pub struct PaymentApiGrpcService {
    domain: Arc<PaymentDomainService>,
}

impl PaymentApiGrpcService {
    /// Wrap the domain service.
    #[must_use]
    pub fn new(domain: Arc<PaymentDomainService>) -> Self {
        Self { domain }
    }

    /// Convenience constructor returning the tonic Server-trait wrapper.
    #[must_use]
    pub fn into_server(self) -> PaymentApiServer<Self> {
        PaymentApiServer::new(self)
    }
}

#[tonic::async_trait]
impl PaymentApi for PaymentApiGrpcService {
    async fn charge(
        &self,
        request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        let ctx = require_security_context(request.metadata())?;
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
        let ctx = require_security_context(request.metadata())?;
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
        let ctx = require_security_context(request.metadata())?;
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
}

/// Bearer-token validation outcome for the inbound gRPC call.
enum AuthOutcome {
    Anonymous,
    Authenticated(SecurityContext),
    /// `authorization` metadata was present but couldn't be turned into a
    /// valid `SecurityContext`. The call must be rejected — silently
    /// degrading to anonymous masks credential bugs in production. Mirror
    /// of the REST handler's behaviour for symmetry across transports.
    Invalid,
}

fn classify_auth(metadata: &tonic::metadata::MetadataMap) -> AuthOutcome {
    let Some(raw) = metadata.get("authorization") else {
        return AuthOutcome::Anonymous;
    };
    let Ok(value) = raw.to_str() else {
        return AuthOutcome::Invalid;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return AuthOutcome::Invalid;
    };
    match SecurityContext::builder()
        .bearer_token(token.to_owned())
        .build()
    {
        Ok(ctx) => AuthOutcome::Authenticated(ctx),
        Err(_) => AuthOutcome::Invalid,
    }
}

/// Produce a [`SecurityContext`] for the gRPC call, rejecting with
/// `Code::Unauthenticated` (canonical Problem trailer attached) when an
/// `authorization` metadata is present but malformed.
fn require_security_context(
    metadata: &tonic::metadata::MetadataMap,
) -> Result<SecurityContext, Status> {
    match classify_auth(metadata) {
        AuthOutcome::Anonymous => Ok(SecurityContext::anonymous()),
        AuthOutcome::Authenticated(ctx) => Ok(ctx),
        AuthOutcome::Invalid => {
            let err = CanonicalError::unauthenticated()
                .with_reason("malformed authorization metadata")
                .create();
            Err(canonical_to_status(err))
        }
    }
}

/// Map a [`CanonicalError`] onto a [`tonic::Status`] **with** the canonical
/// [`Problem`] envelope attached as a binary trailer
/// (`x-modkit-problem-bin`, per [`toolkit_transport_grpc::attach_problem`]).
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
