//! The findings a door's checks collect, and the one rejection they become.
//!
//! Validation is staged, and the first stage that fails answers (P-D-202):
//! the shape parse refuses first, then the door's checks run, each appending
//! to one [`ValidationReport`] that is refused as a whole with every violation
//! of that stage in the answer, then the usage-type resolve runs. A door
//! authorizes before it touches the replay store, so a denied caller consumes
//! no key (P-D-198).

use core::fmt;

use toolkit_macros::domain_model;

/// One finding against a candidate mutation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The wire code this finding rides. Constants live on the rules that raise
    /// them; this is where the rule put its own.
    pub code: &'static str,
    /// The field or subject the finding is about, in the payload's own naming.
    pub subject: String,
    /// What is wrong, for a human reading the response.
    pub detail: String,
}

/// Every finding a door's checks collected.
///
/// The rejection a caller receives carries every violation, in the order the
/// checks produced them (P-D-202).
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    violations: Vec<Violation>,
}

impl ValidationReport {
    /// An empty report.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            violations: Vec::new(),
        }
    }

    /// Record a blocking finding.
    pub fn violate(
        &mut self,
        code: &'static str,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.violations.push(Violation {
            code,
            subject: subject.into(),
            detail: detail.into(),
        });
    }

    /// Whether the checks admitted the mutation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }

    /// Every finding, in the order the rules produced them.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }
}

impl fmt::Display for ValidationReport {
    /// Renders the blocking count. The detail belongs in the response envelope,
    /// not in a log line that would repeat it per rule.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} violation(s)", self.violations.len())
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod validation_tests;
