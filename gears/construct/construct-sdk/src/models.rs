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

/// Who a fact came from.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-client:p1
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FactOrigin {
    /// A connector's record.
    Connector,
    /// What the subject told an agent.
    Agent,
    /// A reviewer's correction.
    Reviewer,
    /// A tenant administrator's fact.
    Administrator,
}

/// A category of a person's profile: one person type.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FactCategory {
    Identity,
    Role,
    Skill,
    Preference,
}

/// One fact of a subject's profile: its id (the graph node key), its one
/// property and that property's value, who it came from, when it was stored,
/// and when its source asserted it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ProfileFact {
    pub fact_id: String,
    pub property: String,
    pub value: serde_json::Value,
    pub origin: FactOrigin,
    pub stored_at: time::OffsetDateTime,
    pub produced_at: Option<time::OffsetDateTime>,
}

impl ProfileFact {
    #[must_use]
    pub fn new(
        fact_id: String,
        property: String,
        value: serde_json::Value,
        origin: FactOrigin,
        stored_at: time::OffsetDateTime,
        produced_at: Option<time::OffsetDateTime>,
    ) -> Self {
        Self {
            fact_id,
            property,
            value,
            origin,
            stored_at,
            produced_at,
        }
    }
}

/// The facts of one category.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ProfileCategory {
    pub category: FactCategory,
    pub facts: Vec<ProfileFact>,
}

impl ProfileCategory {
    #[must_use]
    pub fn new(category: FactCategory, facts: Vec<ProfileFact>) -> Self {
        Self { category, facts }
    }
}

/// A subject's profile by category. A category the caller may not read, or
/// without facts, is not listed.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SubjectProfile {
    pub subject_id: uuid::Uuid,
    pub categories: Vec<ProfileCategory>,
}

impl SubjectProfile {
    #[must_use]
    pub fn new(subject_id: uuid::Uuid, categories: Vec<ProfileCategory>) -> Self {
        Self {
            subject_id,
            categories,
        }
    }
}
