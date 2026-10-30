//! What a [`Backend`](super::Backend) operation fails with (feature 0007 §2.8).

use std::fmt;

use crate::error::OutOfRange;
use crate::sequence::Sequence;

/// Messages name the operation and the rule broken, never a stored value, a
/// setting's value or event content.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// The medium cannot be reached or is not ready yet; the same call may
    /// succeed later.
    #[error("backend unavailable: {reason}")]
    Unavailable { reason: String, detail: String },

    /// A registry entry's settings break a rule of the plugin. Names the
    /// setting and the rule.
    #[error("invalid backend config: {detail}")]
    InvalidConfig { detail: String },

    /// A requested position is not one a cursor may hold in this partition.
    ///
    /// Carries the range the partition admits and which end was crossed, and
    /// not the submitted value - see
    /// [`PositionViolation`](crate::PositionViolation) for why.
    #[error("position outside the admissible range [{floor}, {ceiling}]")]
    OffsetOutOfRange {
        floor: Sequence,
        ceiling: Sequence,
        breached: OutOfRange,
        detail: String,
    },

    #[error("partition not found")]
    PartitionNotFound { detail: String },

    #[error("append failed: {reason}")]
    AppendFailed { reason: String, detail: String },

    #[error("read failed: {reason}")]
    ReadFailed { reason: String, detail: String },

    /// The backend holds a log id the claim or the call does not accept
    /// (feature 0006).
    #[error("log conflict: the backend holds another log id for this topic")]
    LogConflict { detail: String },

    /// The topic has not been provisioned on this backend.
    #[error("topic not provisioned on this backend")]
    NotProvisioned { detail: String },

    #[error("internal: {0}")]
    Internal(String),
}

impl BackendError {
    pub fn unavailable(reason: impl fmt::Display) -> Self {
        Self::Unavailable {
            reason: reason.to_string(),
            detail: String::new(),
        }
    }

    pub fn invalid_config(detail: impl fmt::Display) -> Self {
        Self::InvalidConfig {
            detail: detail.to_string(),
        }
    }

    pub fn append_failed(reason: impl fmt::Display) -> Self {
        Self::AppendFailed {
            reason: reason.to_string(),
            detail: String::new(),
        }
    }

    pub fn read_failed(reason: impl fmt::Display) -> Self {
        Self::ReadFailed {
            reason: reason.to_string(),
            detail: String::new(),
        }
    }

    /// `operation` names the call that found no log for the topic.
    #[must_use]
    pub fn not_provisioned(operation: &str) -> Self {
        Self::NotProvisioned {
            detail: format!("{operation}: the topic has no log on this backend"),
        }
    }

    /// `operation` names the call the stored log id refused.
    #[must_use]
    pub fn log_conflict(operation: &str) -> Self {
        Self::LogConflict {
            detail: format!("{operation}: the backend holds another log id for this topic"),
        }
    }

    /// The `Exact` position check every backend applies the same way.
    #[must_use]
    pub fn out_of_range(requested: Sequence, floor: Sequence, ceiling: Sequence) -> Self {
        Self::OffsetOutOfRange {
            floor,
            ceiling,
            breached: if requested < floor {
                OutOfRange::BelowFloor
            } else {
                OutOfRange::AboveCeiling
            },
            detail: format!("valid positions are [{floor}, {ceiling}]"),
        }
    }
}
