//! Denial reasons (DESIGN §3.1) and the `LicenseDecision.diagnostics` keys.
//!
//! The vocabulary is closed and grows only by addition. A reader that meets a
//! reason it does not know keeps it as [`DenialReason::Unknown`] instead of
//! failing.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `diagnostics` key of the [`DenialReason`].
pub const DIAG_REASON: &str = "reason";
/// `diagnostics` key of the matched grant id.
pub const DIAG_GRANT_ID: &str = "grant_id";
/// `diagnostics` key set to `true` when the decision passed in a grace period.
pub const DIAG_GRACE: &str = "grace";
/// `diagnostics` key of the [`TerminationReason`](crate::models::TerminationReason) of a `terminated` denial.
pub const DIAG_TERMINATION_REASON: &str = "termination_reason";

/// Why a check denied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DenialReason {
    /// No grant in the tenant covers the resource.
    NoGrantForResource,
    /// Grants cover the resource; none covers this subject.
    SubjectNotBound,
    /// From the plugin: the signature is missing, malformed, or fails.
    SignatureInvalid,
    /// From the plugin: the signing key is not trusted.
    KeyUntrusted,
    /// From the plugin: the evidence is past its freshness.
    Stale,
    /// From the plugin: the clock is behind a mark that the plugin keeps.
    ClockRollback,
    /// The root is not accounted for exactly once, or a link of the chain is
    /// not the only live child of its parent.
    Duplicated,
    /// The root or the candidate awaits acceptance.
    AwaitingAcceptance,
    /// The root or the candidate is suspended.
    Suspended,
    /// The root or the candidate is terminated.
    Terminated,
    /// The window has not started.
    NotYetValid,
    /// The window ended and there is no grace period.
    Expired,
    /// The grace period ended.
    GraceExpired,
    /// The candidate does not fit the root fields that `verify` returned.
    ChainInvalid,
    /// A condition is false, or its attribute is absent.
    ConditionUnmet,
    /// Condition evaluation exceeded the per-check budget.
    ConditionBudgetExceeded,
    /// A reason that this version of the SDK does not know.
    Unknown(String),
}

impl DenialReason {
    /// The wire string.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::NoGrantForResource => "no_grant_for_resource",
            Self::SubjectNotBound => "subject_not_bound",
            Self::SignatureInvalid => "signature_invalid",
            Self::KeyUntrusted => "key_untrusted",
            Self::Stale => "stale",
            Self::ClockRollback => "clock_rollback",
            Self::Duplicated => "duplicated",
            Self::AwaitingAcceptance => "awaiting_acceptance",
            Self::Suspended => "suspended",
            Self::Terminated => "terminated",
            Self::NotYetValid => "not_yet_valid",
            Self::Expired => "expired",
            Self::GraceExpired => "grace_expired",
            Self::ChainInvalid => "chain_invalid",
            Self::ConditionUnmet => "condition_unmet",
            Self::ConditionBudgetExceeded => "condition_budget_exceeded",
            Self::Unknown(s) => s,
        }
    }

    /// Parse a wire string. An unknown string gives [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "no_grant_for_resource" => Self::NoGrantForResource,
            "subject_not_bound" => Self::SubjectNotBound,
            "signature_invalid" => Self::SignatureInvalid,
            "key_untrusted" => Self::KeyUntrusted,
            "stale" => Self::Stale,
            "clock_rollback" => Self::ClockRollback,
            "duplicated" => Self::Duplicated,
            "awaiting_acceptance" => Self::AwaitingAcceptance,
            "suspended" => Self::Suspended,
            "terminated" => Self::Terminated,
            "not_yet_valid" => Self::NotYetValid,
            "expired" => Self::Expired,
            "grace_expired" => Self::GraceExpired,
            "chain_invalid" => Self::ChainInvalid,
            "condition_unmet" => Self::ConditionUnmet,
            "condition_budget_exceeded" => Self::ConditionBudgetExceeded,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl Serialize for DenialReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for DenialReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&s))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "reason_tests.rs"]
mod reason_tests;
