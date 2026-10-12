//! Rejections of the retained registry foundation.
use crate::domain::validation::ValidationReport;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A registry operation rejection.
#[domain_model]
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("{code}: {detail}")]
    Conflict { code: &'static str, detail: String },
    #[error("{code}: {detail}")]
    Forbidden { code: &'static str, detail: String },
    #[error("{what} {id} was not found")]
    NotFound { what: &'static str, id: Uuid },
    #[error("approval refused: {0:?}")]
    Approval(ApprovalRefusal),
    #[error("the unit was refreshed; review generation {generation}")]
    StaleUnit { generation: i32 },
    #[error("validation failed: {0}")]
    Validation(ValidationReport),
    #[error("stale revision: expected {expected}, found {found}")]
    StaleRevision {
        /// What the caller pinned.
        expected: i64,
        /// What the head actually carries.
        found: i64,
    },
    #[error("idempotency conflict on key {0}")]
    IdempotencyConflict(String),
    #[error("idempotency key in flight: {0}")]
    IdempotencyKeyInFlight(String),
    #[error("audit unavailable: {0}")]
    AuditUnavailable(String),
    #[error("usage type unresolved: {0}")]
    UsageTypeUnresolved(String),
    #[error("usage type unavailable: {0}")]
    UsageTypeUnavailable(String),
    #[error("usage type catalog refused the caller: {0}")]
    UsageTypeForbidden(String),
    /// Pricing's SKU usage port could not answer a filter on it (P-D-212).
    #[error("SKU usage unavailable: {0}")]
    UsageUnavailable(String),
    #[error("unrecognized metering unit: {0}")]
    UnrecognizedUnit(String),
    #[error("incomplete meter declaration: {0}")]
    MeterDeclarationIncomplete(String),
}

impl DomainError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Conflict { code, .. } | Self::Forbidden { code, .. } => code,
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Approval(refusal) => refusal.code,
            Self::StaleUnit { .. } => "UNIT_STALE",
            Self::Validation(_) => "VALIDATION",
            Self::StaleRevision { .. } => "STALE_REVISION",
            Self::IdempotencyConflict(_) => "IDEMPOTENCY_CONFLICT",
            Self::IdempotencyKeyInFlight(_) => "IDEMPOTENCY_KEY_IN_FLIGHT",
            Self::AuditUnavailable(_) => "AUDIT_UNAVAILABLE",
            Self::UsageTypeUnresolved(_) => "USAGE_TYPE_UNRESOLVED",
            Self::UsageTypeUnavailable(_) => "USAGE_TYPE_UNAVAILABLE",
            Self::UsageTypeForbidden(_) => "USAGE_TYPE_FORBIDDEN",
            Self::UsageUnavailable(_) => "USAGE_UNAVAILABLE",
            Self::UnrecognizedUnit(_) => "UNRECOGNIZED_UNIT",
            Self::MeterDeclarationIncomplete(_) => "METER_DECLARATION_INCOMPLETE",
        }
    }
}

/// Cloneable refusal metadata; typed database failures stay in the door's transaction error.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRefusal {
    pub code: &'static str,
    pub detail: String,
}
// `detail` is the text the wire and the log use. `DomainError` stays `Clone` and `Eq`.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<bss_approval::ApprovalError> for DomainError {
    fn from(error: bss_approval::ApprovalError) -> Self {
        match error {
            // These variants carry a subject-specific code distinct from code(),
            // which names only the engine's broad error category.
            bss_approval::ApprovalError::InvalidSubmit {
                code,
                field,
                detail,
            } => {
                let mut report = ValidationReport::new();
                report.violate(code, field, detail);
                Self::Validation(report)
            }
            // A category a subject's content names that the tenant does not hold is a 404, as
            // at the draft doors; its refusal names the category id (`approvals::require_category`).
            // Without an id (a store refusal) it stays a 409 refusal with its code.
            bss_approval::ApprovalError::ApplyRefused {
                code: "CATEGORY_NOT_FOUND",
                detail,
            } => match Uuid::parse_str(&detail) {
                Ok(id) => Self::NotFound {
                    what: "category",
                    id,
                },
                Err(_) => Self::Conflict {
                    code: "CATEGORY_NOT_FOUND",
                    detail,
                },
            },
            bss_approval::ApprovalError::ApplyRefused { code, detail } => {
                Self::Conflict { code, detail }
            }
            // The unit's 404, as the doors' own pre-load answers it.
            bss_approval::ApprovalError::UnitNotFound { unit_id } => Self::NotFound {
                what: "approval_unit",
                id: unit_id,
            },
            other => Self::Approval(ApprovalRefusal {
                code: other.code(),
                detail: other.to_string(),
            }),
        }
    }
}
