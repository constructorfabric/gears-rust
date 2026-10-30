//! The Orders REST error gear (D-219, ADR-0005): the one place an [`OrdersError`] becomes an
//! RFC 9457 [`Problem`], and the one place a stored Orders Problem becomes an SDK error again.
//!
//! Orders refusals are contract errors (DESIGN *Canonical wire contract*): the canonical
//! category's `type`/`title`, the registered HTTP status ([`wire::reason_status`]), the stable
//! `error_code` under `error_domain = "orders.v1"`, and only the permitted diagnostics in
//! `context.data`. The decode direction ([`RefusalEnvelope`]'s `TryFrom<Problem>`) keys on
//! `(error_domain, error_code)` and reads `context.data`, never the HTTP status: the shape
//! `#[derive(ContractError)]` generates for the cluster SDK (`cluster-sdk/src/convert.rs`). It is
//! hand-written because the derive's encode direction would change the frozen Orders wire (its
//! `detail` is the Rust variant name and its status the category default, where Orders answers
//! the reason token and the registered 412/428), and a derived encoder that must never run would
//! be a trap.
//!
//! Handlers return [`ApiResult`]: a settled outcome (a stored refusal included) is the `Ok`
//! response replayed byte for byte, and every unsettled failure converts once, through
//! `From<OrdersError> for ApiError`, into [`problem`]. The error type is a `Problem` rather than
//! the toolkit `CanonicalError` because an Orders refusal is a contract error whose `error_code`,
//! `error_domain`, `context.data` and registered 412/428 status a `CanonicalError` cannot carry,
//! as the policy-engine gear's `ApiError(Box<Problem>)` does for its contract errors.
use super::throttle::{self, Rejection};
use super::wire;
use axum::response::{IntoResponse, Response};
use bss_orders_sdk::{OrdersError, catalog::Reason, errors::RefusalDiagnostics};
use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};

/// The `error_domain` of every Orders-owned refusal.
pub const ERROR_DOMAIN: &str = "orders.v1";

/// The Problem of a registered refusal: its canonical category, registered status, code and
/// domain, the given `detail`, and `data` as `context.data`.
#[must_use]
pub fn refusal_problem(reason: Reason, detail: &str, data: Value) -> Problem {
    let m = reason.mapping();
    let mut problem = Problem::contract_error(m.category, m.code, ERROR_DOMAIN, detail, data);
    // A registered status may differ from the category default within its class (412, 428).
    problem.status = Some(wire::reason_status(reason));
    problem
}

/// The Problem the engine settles for a refusal (the stored, replayed snapshot): `detail` is the
/// reason token and `data` the refusal's permitted diagnostics.
#[must_use]
pub fn settled_refusal_problem(reason: Reason, data: Value) -> Problem {
    refusal_problem(reason, reason.mapping().reason, data)
}

/// The Problem for an Orders error; a registered refusal carries its registered wire status.
#[must_use]
pub fn problem(error: OrdersError) -> Problem {
    match error {
        OrdersError::Refused(reason) => {
            let detail = match reason {
                Reason::ExpectedVersionRequired => "Expected version is required.",
                Reason::AuthorizationContextChanged => {
                    "Authorization context changed. Refresh the order before retrying."
                }
                _ => reason.mapping().reason,
            };
            refusal_problem(reason, detail, serde_json::json!({}))
        }
        OrdersError::Settled {
            reason,
            diagnostics,
        } => settled_refusal_problem(
            reason,
            serde_json::to_value(diagnostics).unwrap_or_else(|_| serde_json::json!({})),
        ),
        OrdersError::Unavailable => Problem::from(CanonicalError::service_unavailable().create()),
        OrdersError::Integration => Problem::from(
            CanonicalError::internal("Orders authorization integration failure").create(),
        ),
        OrdersError::Internal => {
            Problem::from(CanonicalError::internal("Orders persistence failure").create())
        }
        OrdersError::OutcomeUnknown => Problem::from(
            CanonicalError::service_unavailable()
                .with_detail(
                    "The outcome is unknown. Retry with the same idempotency key to resolve it.",
                )
                .create(),
        ),
        OrdersError::Other { canonical } => Problem::from(canonical),
        // `OrdersError` is `#[non_exhaustive]` (D-220): a disposition this build does not know
        // fails closed as a sanitized internal error, never as a refusal or a success.
        _ => Problem::from(CanonicalError::internal("Orders internal failure").create()),
    }
}

/// A handler's unsettled failure: an Orders error rendered through [`problem`], or a pre-engine
/// throttling refusal (D-185) rendered by [`throttle::rejection_response`].
#[derive(Debug)]
pub enum ApiError {
    /// An Orders error that settled nothing; boxed because a `Problem` is large.
    Orders(Box<Problem>),
    /// The gear-local per-(caller, order) edge limiter refused before the engine.
    Throttled(Rejection),
}

impl From<OrdersError> for ApiError {
    fn from(error: OrdersError) -> Self {
        Self::Orders(Box::new(problem(error)))
    }
}

impl From<Rejection> for ApiError {
    fn from(rejection: Rejection) -> Self {
        Self::Throttled(rejection)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Orders(problem) => problem.into_response(),
            Self::Throttled(rejection) => throttle::rejection_response(rejection),
        }
    }
}

/// The result of an Orders REST handler.
pub type ApiResult<T> = Result<T, ApiError>;

/// A decoded Orders refusal: the registered reason its `(error_domain, error_code)` names and its
/// typed `context.data` diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalEnvelope {
    /// The registered reason.
    pub reason: Reason,
    /// The permitted diagnostics.
    pub diagnostics: RefusalDiagnostics,
}

impl TryFrom<Problem> for RefusalEnvelope {
    /// A Problem that is not a decodable Orders refusal bounces back unchanged, as the derived
    /// `TryFrom<Problem>` of a contract error does.
    type Error = Problem;

    fn try_from(problem: Problem) -> Result<Self, Problem> {
        let reason = match (
            problem.error_domain.as_deref(),
            problem.error_code.as_deref(),
        ) {
            (Some(ERROR_DOMAIN), Some(code)) => Reason::ALL
                .iter()
                .copied()
                .find(|r| r.mapping().code == code),
            _ => None,
        };
        let Some(reason) = reason else {
            return Err(problem);
        };
        let data = problem
            .context
            .get("data")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        match serde_json::from_value(data) {
            Ok(diagnostics) => Ok(Self {
                reason,
                diagnostics,
            }),
            Err(_) => Err(problem),
        }
    }
}

impl From<RefusalEnvelope> for OrdersError {
    fn from(envelope: RefusalEnvelope) -> Self {
        Self::Settled {
            reason: envelope.reason,
            diagnostics: envelope.diagnostics,
        }
    }
}
