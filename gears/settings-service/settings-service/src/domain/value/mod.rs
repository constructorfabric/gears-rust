// Created: 2026-09-06 by Constructor Tech
//! Stored setting values: the rows the resolver walks and the write path fills.

pub mod repo;

use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

pub use repo::ValueRepository;

/// One row of `setting_values`, as the domain sees it.
///
/// Exactly one of `value` and `secret_ref` is present, which the table check
/// guarantees: a secret-classified row holds a reference into the Credential
/// Store and never the plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredValue {
    /// Surrogate identity.
    pub id: Uuid,
    /// The declaration the value belongs to.
    pub declaration_id: Uuid,
    /// The tenant whose scope the value is set at; the root tenant's id is
    /// platform scope.
    pub tenant_id: Uuid,
    /// The inline value, for every classification but `secret`.
    pub value: Option<Value>,
    /// The Credential Store reference, for a `secret` value.
    pub secret_ref: Option<String>,
    /// Denormalized from the declaration, re-synced when it changes.
    pub data_classification: String,
    /// Whether the value no longer validates against the current type.
    pub needs_review: bool,
    /// Why, when it does not.
    pub needs_review_detail: Option<String>,
    /// When the value last changed — the value arm of the recency indicator.
    pub last_change_at: OffsetDateTime,
    /// Row version, which the value state tag is derived from.
    pub updated_at: OffsetDateTime,
    /// Who set it.
    pub set_by: String,
}

/// What a write supplies to create a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueDraft {
    /// The declaration the value belongs to.
    pub declaration_id: Uuid,
    /// The scope, as a tenant id.
    pub tenant_id: Uuid,
    /// The inline value; `None` for a secret.
    pub value: Option<Value>,
    /// The Credential Store reference; `None` unless the setting is secret.
    pub secret_ref: Option<String>,
    /// Copied from the declaration at write time.
    pub data_classification: String,
    /// Whether the value is written already flagged, as an upgrade copy is.
    pub needs_review: bool,
    /// The flag's detail.
    pub needs_review_detail: Option<String>,
    /// Who set it.
    pub set_by: String,
}
