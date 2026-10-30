//! Stable admission failure codes used in stored outcomes and metrics.

use toolkit_macros::domain_model;

/// The candidate-refusal vocabulary is the SDK's: the registry persists, emits and labels
/// metrics with the same codes its consumers dispatch on (ADR 0005 rule 6).
pub use types_registry_sdk::item_failure::AdmissionFailureReason;

/// Stable delivery-level failure codes.
/// Candidate failures are [`AdmissionFailureReason`]s.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryFailure {
    /// Admission failed with a `WorkerError` code.
    Admission(&'static str),
    /// The envelope declared a `payload_type` this queue does not handle.
    UnexpectedPayloadType,
    /// The message body is not an operation UUID.
    InvalidOperationPayload,
    /// A non-worker service failure; details remain in operator logs.
    ServiceFailure,
    /// The operation row disappeared before exhausted delivery could finish.
    OperationNotFound,
    /// The delivery budget was spent and the operation is still not terminal.
    DeliveryBudgetExhausted,
    /// Admission exceeded its deadline before the delivery budget was exhausted.
    AdmissionDeadlineExceeded,
}

impl DeliveryFailure {
    /// Return the stable `error_code` string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admission(code) => code,
            Self::UnexpectedPayloadType => "unexpected_payload_type",
            Self::InvalidOperationPayload => "invalid_operation_payload",
            Self::ServiceFailure => "admission_service_failure",
            Self::OperationNotFound => "operation_not_found",
            Self::DeliveryBudgetExhausted => "delivery_budget_exhausted",
            Self::AdmissionDeadlineExceeded => "admission_deadline_exceeded",
        }
    }
}

impl std::fmt::Display for DeliveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "reasons_tests.rs"]
mod reasons_tests;
