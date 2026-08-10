//! Failure categories for service-principal operations: clean vendor
//! failures (no state retained) vs ambiguous transport outcomes (state
//! may have been retained).

use std::fmt;

/// The taxonomy is intentionally closed (no `#[non_exhaustive]`): consumers must handle every category explicitly.
#[derive(Debug)]
pub enum ServicePrincipalFailure {
    /// Rejected with no vendor state retained by this call (bad name,
    /// scope not in allowlist, quota exceeded, client id taken).
    /// Permanent — do not retry the same input.
    InvalidInput {
        detail: String,
        field: Option<String>,
    },
    /// Target client absent on the vendor side (404/410) or not owned
    /// by the addressed tenant. Success-equivalent for revoke.
    NotFound { detail: String },
    /// Vendor call failed cleanly — no state retained. Retry is harmless, though not necessarily productive.
    CleanFailure { detail: String },
    /// Transport uncertainty — vendor may have retained state. Never
    /// reported as success.
    Ambiguous { detail: String },
}

impl ServicePrincipalFailure {
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotFound { .. } => "not_found",
            Self::CleanFailure { .. } => "clean_failure",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::InvalidInput { detail, .. }
            | Self::NotFound { detail }
            | Self::CleanFailure { detail }
            | Self::Ambiguous { detail } => detail,
        }
    }
}

impl fmt::Display for ServicePrincipalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.as_metric_label(), self.detail())
    }
}

impl std::error::Error for ServicePrincipalFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_labels_are_stable() {
        let cases = [
            (
                ServicePrincipalFailure::InvalidInput {
                    detail: String::new(),
                    field: None,
                },
                "invalid_input",
            ),
            (
                ServicePrincipalFailure::NotFound {
                    detail: String::new(),
                },
                "not_found",
            ),
            (
                ServicePrincipalFailure::CleanFailure {
                    detail: String::new(),
                },
                "clean_failure",
            ),
            (
                ServicePrincipalFailure::Ambiguous {
                    detail: String::new(),
                },
                "ambiguous",
            ),
        ];
        for (f, label) in cases {
            assert_eq!(f.as_metric_label(), label);
        }
    }

    #[test]
    fn display_shape_is_stable() {
        assert_eq!(
            ServicePrincipalFailure::NotFound { detail: "x".into() }.to_string(),
            "not_found: x"
        );
    }
}
