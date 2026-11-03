//! The closed fulfillment failure reasons (06 §4.4, D-136, D-172, D-182). The audit
//! `acknowledge-failed` rule and the `OrderFulfillmentFailed` event payload and schema read this
//! one declaration.
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;

/// The closed failure-reason enumeration (06 §4.4, D-136, D-172, D-182).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureReason {
    /// The order market diverged at the activation re-check.
    MarketDivergence,
    /// A binding deadline of the order elapsed.
    OrderBindingExpired,
    /// Activation collides with an existing subscription overlap.
    OverlapCollision,
    /// A tenant party's identity facts could not be obtained.
    IdentityPartyUnavailable,
    /// Subscription overlap presence could not be evaluated.
    OverlapPresenceUnevaluable,
    /// A line's fulfillment execution failed.
    LineExecutionFailed,
    /// Withdrawn as an emitted value (D-172); retained so historical payloads validate.
    DependencyGraphInvalid,
    /// Operators forced the failure with compensation unreconciled (D-182).
    OperatorForcedUnreconciled,
}
impl FailureReason {
    /// Every value, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::MarketDivergence,
        Self::OrderBindingExpired,
        Self::OverlapCollision,
        Self::IdentityPartyUnavailable,
        Self::OverlapPresenceUnevaluable,
        Self::LineExecutionFailed,
        Self::DependencyGraphInvalid,
        Self::OperatorForcedUnreconciled,
    ];
    /// The kebab-case wire token (the serde name).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::MarketDivergence => "market-divergence",
            Self::OrderBindingExpired => "order-binding-expired",
            Self::OverlapCollision => "overlap-collision",
            Self::IdentityPartyUnavailable => "identity-party-unavailable",
            Self::OverlapPresenceUnevaluable => "overlap-presence-unevaluable",
            Self::LineExecutionFailed => "line-execution-failed",
            Self::DependencyGraphInvalid => "dependency-graph-invalid",
            Self::OperatorForcedUnreconciled => "operator-forced-unreconciled",
        }
    }
    /// The reason a registered [`token`](Self::token) names; `None` for any other spelling.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.token() == token)
    }
    /// Whether a committed `acknowledge-failed` may record this reason as its `caller_reason`
    /// (06 §4.4, D-136/D-172). `dependency-graph-invalid` stays accepted for historical
    /// payloads; `operator-forced-unreconciled` is fixed by `force-fail-unreconciled` and never
    /// accepted on an acknowledgement (D-182).
    #[must_use]
    pub const fn acknowledgeable(self) -> bool {
        !matches!(self, Self::OperatorForcedUnreconciled)
    }
}
