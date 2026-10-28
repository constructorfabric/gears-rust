//! Orders SDK error surface: closed Orders refusals plus a typed projection of
//! [`CanonicalError`] (ADR-0005 `cpt-cf-errors-adr-sdk-canonical-projection`, D-219).
//!
//! [`OrdersError`] is transport-neutral: it carries the registered [`Reason`] and, for a refusal
//! the engine settled under an idempotency key, the typed [`RefusalDiagnostics`] Orders permits
//! in `context.data`. It never carries an RFC 9457 `Problem`, an HTTP status or a route. The
//! gear's REST layer owns the Problem encoding and the reason → HTTP status table, and decodes a
//! stored Orders Problem back into [`OrdersError::Settled`] keyed on
//! `(error_domain, error_code)` plus `context.data` (the `#[derive(ContractError)]` shape of the
//! cluster SDK precedent).
//!
//! # Why the trait keeps `OrdersError`
//!
//! ADR-0005 allows the projection form: [`OrdersV1`](crate::OrdersV1) returns this enum, which
//! converts infallibly from [`CanonicalError`] with the mandatory [`OrdersError::Other`]
//! catch-all. `CanonicalError` cannot carry an Orders refusal: Orders refusals are contract
//! errors (`error_code`/`error_domain` with `context.data`, DESIGN *Canonical wire contract*),
//! and `TryFrom<Problem> for CanonicalError` neither keeps `error_code` nor `context.data`, nor
//! accepts the contract-error context for most categories (D-219).
//!
//! # Consumer dispatch reference
//!
//! | Disposition | Match arm |
//! |---|---|
//! | refused before any settlement (boundary/PEP), no diagnostics | [`OrdersError::Refused`] |
//! | refusal settled under the key; same key replays it — inspect [`RefusalDiagnostics`] | [`OrdersError::Settled`] |
//! | Orders or a required dependency unavailable; retryable | [`OrdersError::Unavailable`] |
//! | authorization/provider integration failure; fails closed, not retryable | [`OrdersError::Integration`] |
//! | rolled-back persistence failure | [`OrdersError::Internal`] |
//! | commit acknowledgement lost; retry with the same key | [`OrdersError::OutcomeUnknown`] |
//! | any other canonical error (forward-compat) | [`OrdersError::Other`] |
//!
//! [`OrdersError::reason`] answers the registered reason for either refusal arm.
//!
//! The enum is `#[non_exhaustive]` (D-220): a later Orders may add a disposition, so every
//! `match` outside this crate ends with a wildcard arm. Treat that arm as the fail-closed
//! default, as the gear's REST layer does (an internal failure, never a refusal or a success).
//!
//! # Consumer integration
//!
//! Propagate with `?`, match on the arms above, or chain through
//! `impl From<OrdersError> for OwnError`. A consumer that holds a [`CanonicalError`] from
//! another Orders-adjacent call projects it with `OrdersError::from(canonical)`.
use crate::catalog::{OrderState, Reason, Trigger};
use crate::models::{DraftRevision, OrderVersion};
use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// Orders SDK failure. Unavailability never settles a business refusal.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum OrdersError {
    /// A registered refusal decided before anything settled (boundary validation, the PEP, a
    /// read refusal). It carries no diagnostics.
    #[error("Orders request refused: {0:?}")]
    Refused(Reason),
    /// A refusal the engine settled under the idempotency key: its registered reason plus the
    /// permitted `context.data` diagnostics, exactly as REST returns and replays them (DESIGN
    /// *Workflow SDK contract*: "returned through the canonical SDK error mapping with its
    /// registered reason and allowed diagnostics").
    #[error("Orders request refused: {reason:?}")]
    Settled {
        reason: Reason,
        diagnostics: RefusalDiagnostics,
    },
    #[error("Orders is not available")]
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
    /// Catch-all for a canonical error Orders does not model (ADR-0005): the full
    /// [`CanonicalError`] is preserved for inspection and forward compatibility.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
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

    /// The permitted diagnostics of a settled refusal.
    #[must_use]
    pub fn diagnostics(&self) -> Option<&RefusalDiagnostics> {
        match self {
            Self::Settled { diagnostics, .. } => Some(diagnostics),
            _ => None,
        }
    }
}

/// Infallible projection (ADR-0005 rule 3). An Orders refusal never arrives as a bare
/// [`CanonicalError`] (it is a contract error, decoded by the gear REST layer), so only the two
/// infrastructure categories Orders itself answers are named; every other category is
/// [`OrdersError::Other`] with the full canonical error.
impl From<CanonicalError> for OrdersError {
    fn from(canonical: CanonicalError) -> Self {
        match canonical {
            CanonicalError::ServiceUnavailable { .. } => Self::Unavailable,
            CanonicalError::Internal { .. } => Self::Internal,
            canonical => Self::Other { canonical },
        }
    }
}

/// The permitted `context.data` members of a settled Orders refusal (DESIGN *Canonical wire
/// contract*: "only explicitly permitted variant fields"). Every member is optional: which ones a
/// refusal carries depends on its reason, and a refusal without diagnostics is the empty object.
/// Unknown members are ignored, so a newer Orders adding a permitted member stays decodable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalDiagnostics {
    /// `not-admissible`: the order's state when the trigger was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<OrderState>,
    /// `not-admissible`: the refused trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    /// `version-conflict`, `precondition-failed`: the order's current commercial version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<OrderVersion>,
    /// `version-conflict`, `precondition-failed` on a draft operation: the current draft
    /// revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<DraftRevision>,
    /// `second-approver-required`: the refusal's own audit entry, which a second operator names
    /// to approve (D-182).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_audit_id: Option<Uuid>,
    /// `overlap-collision`: the blocked overlap tuples (D-179).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<Vec<BlockedOverlap>>,
}

/// One blocked overlap tuple of an `overlap-collision` refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedOverlap {
    /// The colliding overlap scope key.
    pub overlap_scope_key: String,
    /// The conflicting order, only where the caller's read scope can see it (D-179); `null`
    /// otherwise.
    pub conflicting_order_id: Option<Uuid>,
}
