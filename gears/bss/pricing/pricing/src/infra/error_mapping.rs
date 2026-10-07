//! Shared transport errors and their canonical wire mapping.

use toolkit::api::canonical_prelude::{CanonicalError, resource_error};

#[resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct PricingResource;

/// Errors still constructed by the retained request plumbing.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    /// A request body or header cannot be interpreted.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A readable body carries a value no pricing field may hold: 400 `VALIDATION`.
    #[error("invalid {field}: {detail}")]
    Validation { field: String, detail: String },
    /// Internal serialization or middleware wiring failure.
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<DomainError> for CanonicalError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::InvalidRequest(detail) => PricingResource::invalid_argument()
                .with_constraint(detail)
                .create(),
            DomainError::Validation { field, detail } => PricingResource::invalid_argument()
                .with_field_violation(field, detail, "VALIDATION")
                .create(),
            DomainError::Internal(detail) => {
                CanonicalError::internal(format!("pricing: {detail}")).create()
            }
        }
    }
}

impl From<crate::domain::RuleError> for CanonicalError {
    fn from(error: crate::domain::RuleError) -> Self {
        match error.reason {
            Some(reason) => reason.into(),
            None => PricingResource::invalid_argument()
                .with_field_violation(
                    crate::domain::price::field_of(error.code),
                    error.code,
                    error.code,
                )
                .create(),
        }
    }
}
