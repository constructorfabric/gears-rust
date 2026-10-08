//! Closed Orders refusals and lossless unknown downstream Problems.
use crate::catalog::Reason;
use toolkit_canonical_errors::{CanonicalError, Problem};

/// SDK failure. Unavailability never settles a business refusal.
#[derive(Debug, thiserror::Error)]
pub enum OrdersError {
    #[error("Orders request refused: {0:?}")]
    Refused(Reason),
    /// A refusal the engine settled under the idempotency key: its registered reason plus the
    /// stored canonical Problem with its permitted `context.data` diagnostics (for example
    /// `current_version`/`draft_revision` on `version-conflict`), exactly as REST returns and
    /// replays it (DESIGN *Workflow SDK contract*: "returned through the canonical SDK error
    /// mapping with its registered reason and allowed diagnostics").
    #[error("Orders request refused: {reason:?}")]
    Settled {
        reason: Reason,
        problem: Box<Problem>,
    },
    #[error("Orders Lifecycle is not available")]
    Unavailable,
    /// Authorization/provider integration failure (for example invalid or missing PDP
    /// constraints). Fails closed; not a retryable outage and not a business refusal.
    #[error("Orders integration failure")]
    Integration,
    /// An unexpected persistence failure after the attempt was fully rolled back: sanitized
    /// canonical `Internal` (500), never a durably recorded refusal (Foundation
    /// *Infrastructure-error termination*).
    #[error("Orders internal failure")]
    Internal,
    /// The commit acknowledgement was lost: the attempt may or may not have committed. This is
    /// never a confirmed rollback; a retry with the same idempotency key resolves it.
    #[error("Orders outcome unknown")]
    OutcomeUnknown,
    #[error("Remote Orders failure")]
    Remote(Box<Problem>),
}

impl OrdersError {
    /// The registered refusal reason, whether refused at the boundary or settled.
    #[must_use]
    pub fn reason(&self) -> Option<Reason> {
        match self {
            Self::Refused(reason) | Self::Settled { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    /// Produce the shared canonical envelope. The REST adapter applies the 428 exception.
    #[must_use]
    pub fn into_problem(self) -> Problem {
        match self {
            Self::Refused(reason) => {
                let m = reason.mapping();
                let detail = match reason {
                    Reason::ExpectedVersionRequired => "Expected version is required.",
                    Reason::AuthorizationContextChanged => {
                        "Authorization context changed. Refresh the order before retrying."
                    }
                    _ => m.reason,
                };
                Problem::contract_error(
                    m.category,
                    m.code,
                    "orders-lifecycle.v1",
                    detail,
                    serde_json::json!({}),
                )
            }
            Self::Unavailable => Problem::from(CanonicalError::service_unavailable().create()),
            Self::Integration => Problem::from(
                CanonicalError::internal("Orders authorization integration failure").create(),
            ),
            Self::Internal => {
                Problem::from(CanonicalError::internal("Orders persistence failure").create())
            }
            Self::OutcomeUnknown => Problem::from(
                CanonicalError::service_unavailable()
                    .with_detail(
                        "The outcome is unknown. Retry with the same idempotency key to resolve it.",
                    )
                    .create(),
            ),
            Self::Settled { problem, .. } | Self::Remote(problem) => *problem,
        }
    }
}
