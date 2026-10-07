//! Reversible `CanonicalError` item failures (DESIGN §3.3).
//! `FailedPrecondition` violation 0 carries reason/key/message; later violations carry
//! context.<name>/value/empty description. Unknown reasons and context round-trip unchanged.

use std::collections::BTreeMap;

use toolkit_canonical_errors::CanonicalError;

/// Prefix of a context violation's `type`.
const CONTEXT_PREFIX: &str = "context.";

/// Reason codes the SDK acts on. Others are carried as they arrive.
pub mod reason {
    pub const ALREADY_EXISTS: &str = "already_exists";
    pub const PRECONDITION_FAILED: &str = "precondition_failed";
    pub const DEPENDENCY_NOT_FOUND: &str = "dependency_not_found";
    pub const BLOCKED_BY_DEPENDENCY: &str = "blocked_by_dependency";
    pub const BLOCKED_BY_PREDECESSOR: &str = "blocked_by_predecessor";
    pub const MISSING_PREDECESSOR: &str = "missing_predecessor";
    pub const SYSTEM_FAILURE: &str = "system_failure";
    /// Newer release of the same publisher (D18, Phase 9).
    pub const SUPERSEDED: &str = "superseded";
    /// Another publisher owns the entity (D18, Phase 9).
    pub const PUBLISHER_MISMATCH: &str = "publisher_mismatch";
}

/// Context names the SDK reads. Others are carried as they arrive.
pub mod context {
    pub const DEPENDENCY_ID: &str = "dependency_id";
    pub const DEPENDENCY_KIND: &str = "dependency_kind";
    pub const DIAGNOSTIC_CODE: &str = "diagnostic_code";
    pub const STORED_VERSION: &str = "stored_version";
    pub const OFFERED_VERSION: &str = "offered_version";
    pub const STORED_PUBLISHER: &str = "stored_publisher";
    pub const OFFERED_PUBLISHER: &str = "offered_publisher";
}

/// One item's failure as the registry records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionFailure {
    pub reason: String,
    pub message: String,
    pub context: BTreeMap<String, String>,
}

impl AdmissionFailure {
    /// A failure with no context.
    #[must_use]
    pub fn new(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            message: message.into(),
            context: BTreeMap::new(),
        }
    }

    /// Adds one context entry.
    #[must_use]
    pub fn with_context(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(name.into(), value.into());
        self
    }

    /// The context entry `name`, if present.
    #[must_use]
    pub fn context(&self, name: &str) -> Option<&str> {
        self.context.get(name).map(String::as_str)
    }

    /// Encodes this failure of the item `key` (its canonical spelling).
    #[must_use]
    pub fn into_canonical(self, key: &str) -> CanonicalError {
        let mut builder = crate::gts::TypeResource::failed_precondition()
            .with_resource(key)
            .with_precondition_violation(key, self.message, self.reason);
        for (name, value) in self.context {
            builder =
                builder.with_precondition_violation(value, "", format!("{CONTEXT_PREFIX}{name}"));
        }
        builder.create()
    }

    /// Decode errors from [`Self::into_canonical`]; None for other shapes.
    #[must_use]
    pub fn from_canonical(error: &CanonicalError) -> Option<Self> {
        let CanonicalError::FailedPrecondition { ctx, .. } = error else {
            return None;
        };
        let (primary, rest) = ctx.violations.split_first()?;
        let mut failure = Self::new(&primary.type_, &primary.description);
        for violation in rest {
            let name = violation.type_.strip_prefix(CONTEXT_PREFIX)?;
            failure
                .context
                .insert(name.to_owned(), violation.subject.clone());
        }
        Some(failure)
    }
}

#[cfg(test)]
#[path = "item_failure_tests.rs"]
mod item_failure_tests;
