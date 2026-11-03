//! Public models of the Construct gear.
//!
//! Transport-agnostic data structures that define the contract between the
//! gear and its consumers. `#[domain_model]` keeps infrastructure types out of
//! them at compile time.

use toolkit_macros::domain_model;

/// What record intake answers for a record it takes. A refused record is an
/// error instead: it names the record's type, the place in the record and the
/// broken rule.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-client:p1
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordOutcome {
    /// The record passed the checks and is taken for processing. It is not
    /// stored: processing may still drop it, and then nothing from it is kept.
    Received,
    /// A record with the same identity was received before. Nothing changes.
    Repeat,
}
