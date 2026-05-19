use modkit_canonical_errors::{CanonicalError, resource_error};

pub type UsageCollectorError = CanonicalError;

/// Resource-scoped error type for usage record operations.
#[resource_error("gts.cf.core.usage.record.v1~")]
pub struct UsageRecordError;

/// Resource-scoped error type for module configuration operations.
#[resource_error("gts.cf.core.usage.module_config.v1~")]
pub struct ModuleConfigError;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
