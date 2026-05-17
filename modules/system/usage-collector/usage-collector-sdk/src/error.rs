use modkit_canonical_errors::{CanonicalError, resource_error};

/// Top-level error returned by the usage-collector SDK trait surface.
///
/// Alias of `modkit_canonical_errors::CanonicalError`; pattern-match on its
/// variants (e.g. `UsageCollectorError::PermissionDenied { .. }`) directly.
pub type UsageCollectorError = CanonicalError;

/// Resource-scoped error builder for usage record operations.
#[resource_error("gts.cf.core.usage.record.v1~")]
pub struct UsageRecordError;

/// Resource-scoped error builder for module configuration operations.
#[resource_error("gts.cf.core.usage.module_config.v1~")]
pub struct ModuleConfigError;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
