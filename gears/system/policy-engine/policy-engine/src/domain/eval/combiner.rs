//! Combination of per-document results: an enforced denial denies, a denial of
//! a non-enforcing assignment is only a shadow denial. Every applicable
//! document is evaluated, so every denial is collected.

use toolkit_macros::domain_model;

use crate::domain::model::{BundleId, DocumentId, VersionId};

/// Identity of an evaluated document.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvaluatedDocumentKey {
    /// The document.
    pub document_id: DocumentId,
    /// The document's name.
    pub document_name: String,
    /// The version holding the document.
    pub version_id: VersionId,
    /// The version's bundle.
    pub bundle_id: BundleId,
}

/// One document's verdict.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedDocument {
    /// Which document.
    pub key: EvaluatedDocumentKey,
    /// Whether its assignment enforces.
    pub enforce: bool,
    /// Whether `deny` held.
    pub denied: bool,
}

/// The combined result of one request.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CombinedResult {
    /// Denials of enforcing assignments, in evaluation order.
    pub denials: Vec<EvaluatedDocumentKey>,
    /// Denials of non-enforcing assignments, in evaluation order.
    pub shadow_denials: Vec<EvaluatedDocumentKey>,
}

impl CombinedResult {
    /// Whether an enforced denial stands.
    #[must_use]
    pub fn is_denied(&self) -> bool {
        !self.denials.is_empty()
    }
}

/// Splits the denying documents into denials and shadow denials.
#[must_use]
pub fn combine(evaluated: Vec<EvaluatedDocument>) -> CombinedResult {
    let mut result = CombinedResult::default();
    for doc in evaluated.into_iter().filter(|d| d.denied) {
        if doc.enforce {
            result.denials.push(doc.key);
        } else {
            result.shadow_denials.push(doc.key);
        }
    }
    result
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "combiner_tests.rs"]
mod combiner_tests;
