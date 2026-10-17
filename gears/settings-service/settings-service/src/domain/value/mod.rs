// Created: 2026-09-06 by Virtuozzo International GmbH
//! Stored setting values: the rows the resolver walks and the write path fills.

pub mod repo;

use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::visibility::DomainVisibility;

pub use repo::ValueRepository;

/// What the review listing is asked with, every decision already taken by the
/// caller: the handler decides, the repository only applies.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ReviewRequest<'a> {
    /// The narrowed grant, pushed into the declarations the rows must belong to.
    pub scope: &'a AccessScope,
    /// The domains the caller may see.
    pub visibility: &'a DomainVisibility,
    /// The caller's root-to-self chain: a declaration `hidden` on it has no
    /// rows on the page.
    pub hidden_for: &'a [Uuid],
    /// The target and its non-standalone descendants: where a flagged row may
    /// be listed.
    pub tenant_ids: &'a [Uuid],
    /// The declaration-selecting `$filter` remainder: `category_id`, `key`.
    pub declarations: Option<&'a toolkit_odata::ast::Expr>,
    /// Page size, cursor and the binding the cursor must carry; its own filter
    /// and order are not read — the listing's order is fixed.
    pub query: &'a ODataQuery,
}

/// The binding of a review cursor: the `$filter` as asked and the target, so a
/// cursor continues only the listing it was minted for.
///
/// Compared, never stored: it is a guard against continuing one listing with
/// another's cursor, not a secret.
#[must_use]
pub fn review_binding(target: Uuid, filter: Option<&toolkit_odata::ast::Expr>) -> String {
    // The library's normalised text, so spelling, spacing and the order of
    // equal operands do not tell two equal filters apart, and the digest does
    // not move with a compiler release: FNV-1a over fixed constants.
    let text = format!(
        "{target}|{}",
        filter.map_or_else(String::new, toolkit_odata::normalize_filter_for_hash)
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("{hash:016x}")
}

/// One row of `setting_values`, as the domain sees it.
///
/// Exactly one of `value` and `secret_ref` is present, which the table check
/// guarantees: a secret-classified row holds a reference into the Credential
/// Store and never the plaintext.
#[domain_model]
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
#[domain_model]
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

#[cfg(test)]
#[path = "binding_tests.rs"]
mod binding_tests;
