//! Distinct document roles prevent transposing an asymmetric comparison.
//! Explicit constructors identify each side; no `From<&Value>` or `Deref`.

use serde_json::Value;

/// The **old** side: the definition a candidate is measured against.
#[derive(Clone, Copy, Debug)]
pub struct BaselineDoc<'a>(&'a Value);

/// The **new** side: the document under admission.
#[derive(Clone, Copy, Debug)]
pub struct CandidateDoc<'a>(&'a Value);

impl<'a> BaselineDoc<'a> {
    /// Name this document the baseline.
    pub const fn new(document: &'a Value) -> Self {
        Self(document)
    }

    /// The wrapped document, for the one call that hands it to `gts-rust`.
    pub const fn get(self) -> &'a Value {
        self.0
    }
}

impl<'a> CandidateDoc<'a> {
    /// Name this document the candidate.
    pub const fn new(document: &'a Value) -> Self {
        Self(document)
    }

    /// The wrapped document, for the one call that hands it to `gts-rust`.
    pub const fn get(self) -> &'a Value {
        self.0
    }
}
