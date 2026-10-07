use toolkit_canonical_errors::{CanonicalError, resource_error};

/// Validation failure of a local [`crate::contracts::ExecutionContract`] helper.
///
/// Contract helpers run in-process before any trait call, so they keep this
/// narrow type (ADR-0005 "Non-Canonical Methods"). Convert with `?` or
/// [`CanonicalError::from`] to get the canonical `InvalidArgument`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid execution definition: {message}")]
pub struct DefinitionError {
    message: String,
}
impl DefinitionError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

// Literal MUST equal `crate::gts::DEFINITION_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.durable_execution.execution.definition.v1~"))]
struct DefinitionResource;

impl From<DefinitionError> for CanonicalError {
    fn from(e: DefinitionError) -> Self {
        DefinitionResource::invalid_argument()
            .with_field_violation("definition", e.message, crate::reason::INVALID_DEFINITION)
            .create()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/error_tests.rs"]
mod tests;

#[resource_error(gts_id!("cf.durable_execution.execution.run.v1~"))]
struct RunResource;

pub fn invalid_input(field: &str, message: &str) -> CanonicalError {
    RunResource::invalid_argument()
        .with_field_violation(field, message, crate::reason::INVALID_FORMAT)
        .create()
}
