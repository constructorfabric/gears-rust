//! Cross-field search over the settings hub: what may be matched, and how.
//!
//! Search runs over **stored rows**, never resolved values (DESIGN §4.2
//! *Search*): a declaration's key, display name, description and category
//! name, its Schema Default, and the overrides explicitly set inside the caller's visible
//! subtree. Authorization is applied to the **corpus** before matching, not to
//! the output, because a match's existence, a count and a hit each disclose
//! content on their own. This module holds the vocabulary that rule is stated
//! in; [`service`] applies it and the repository binding runs the queries.

pub mod needle;
pub mod service;

use std::hash::{DefaultHasher, Hash, Hasher};
use toolkit_macros::domain_model;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_db::secure::DBRunner;
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::Category;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::odata::Listing;
use crate::domain::value::StoredValue;

pub use needle::{Corpus, MAX_QUERY_CHARS, MIN_QUERY_CHARS, Needle, like_escape};

/// Which field of a hit matched, in the order a client is told about them.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchedField {
    Key,
    DisplayName,
    Description,
    CategoryName,
    DefaultValue,
    Value,
}

impl MatchedField {
    /// The wire spelling, from the contract's closed vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::DisplayName => "display_name",
            Self::Description => "description",
            Self::CategoryName => "category_name",
            Self::DefaultValue => "default_value",
            Self::Value => "value",
        }
    }
}

/// The text a JSON value is matched as: a string scalar as itself, anything
/// else as its JSON text — the same projection the database indexes with
/// `value #>> '{}'`.
#[must_use]
pub fn text_projection(value: &Value) -> String {
    // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-2
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
    // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-2
}

/// The token a search's pagination cursor is bound to: the same query, target,
/// corpus and filter mint the same token, and a cursor carrying another is
/// refused — a cursor minted under one filter would otherwise page on under
/// another, silently. Not a security boundary — the corpus is re-decided on
/// every request — only a guard against continuing one search with another's
/// cursor.
#[must_use]
pub fn cursor_binding(
    needle: &Needle,
    tenant: Uuid,
    corpus: Corpus,
    filter: Option<&toolkit_odata::ast::Expr>,
) -> String {
    let mut hasher = DefaultHasher::new();
    needle.as_str().hash(&mut hasher);
    tenant.hash(&mut hasher);
    corpus.hash(&mut hasher);
    // The parsed expression, so spelling and spacing do not tell two equal
    // filters apart.
    format!("{filter:?}").hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What a search is asked with, every decision already taken by the caller:
/// the handler decides, the service and the binding only apply.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct SearchRequest<'a> {
    /// The narrowed grant, pushed into the declarations query.
    pub scope: &'a AccessScope,
    /// The domains the caller may see.
    pub visibility: &'a DomainVisibility,
    /// The validated query.
    pub needle: &'a Needle,
    /// The classifications the caller may match against.
    pub corpus: Corpus,
    /// The tenant the search is made at: where `flagged_only` looks.
    pub target: Uuid,
    /// The target and its non-standalone descendants: where an override may
    /// be matched.
    pub tenant_ids: &'a [Uuid],
    /// The caller's root-to-self chain: a declaration `hidden` on it is left
    /// out in the page query, so the page comes back full.
    pub hidden_for: &'a [Uuid],
    /// The most matching override rows a page fetches; more than that and the
    /// search is refused rather than cut short of its hits.
    pub override_limit: usize,
    /// `needs_review eq true`: only settings whose own override at the target
    /// (`target`) is flagged for review: a narrowing in the page query, the
    /// one the browse applies.
    pub flagged_only: bool,
    /// Page size, cursor, the declaration-selecting `$filter` remainder and
    /// the binding the cursor must carry.
    pub query: &'a ODataQuery,
}

/// The most matching override rows one page fetches. A page of settings is
/// bounded by `limit`; its override hits were not, and a broad needle over a
/// wide subtree could return thousands of rows for one page. Past this bound
/// the search is refused with the bound named: a client lowers `limit`,
/// narrows the needle or the target.
pub const SEARCH_OVERRIDE_LIMIT: usize = 1_000;

/// The refusal a page answers when its matching overrides exceed `limit`.
#[must_use]
pub fn too_many_hits(limit: usize) -> DomainError {
    DomainError::Validation {
        field: "limit".to_owned(),
        code: crate::field::SEARCH_TOO_MANY_HITS,
        message: format!(
            "more than {limit} overrides match on this page; lower `limit`, or narrow `q` or \
             `tenant`"
        ),
    }
}

/// The port the search runs through. The binding owns the SQL dialect; the
/// domain states what a query means.
#[async_trait]
pub trait SearchRepository: Send + Sync {
    /// A page of **active** declarations, ordered by key, that match the
    /// request's needle on key, display name, description, the name of their category, their
    /// Schema Default (within the corpus and not JSON `null`), or an override
    /// explicitly set at one of the request's tenants (within the corpus,
    /// never a secret row). The scope and visibility narrow the page as they
    /// narrow browsing; so do the query's `$filter` on `key` and
    /// `category_id`, and `flagged_only`; the query carries `limit`, `cursor`
    /// and the `filter_hash` the cursor is bound to. The listing carries how
    /// many declarations match in all.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for a cursor that does not decode or was
    /// minted for another binding; [`DomainError::Internal`] when the database
    /// fails.
    async fn declarations<C: DBRunner>(
        &self,
        conn: &C,
        request: &SearchRequest<'_>,
    ) -> Result<Listing<Declaration>, DomainError>;

    /// The overrides of `declaration_ids` set at `tenant_ids` whose text
    /// projection matches `needle`, within `corpus`; never a secret row and
    /// never a subject-scoped row. Ordered by declaration, then tenant, and at
    /// most `limit + 1` rows — one past the bound, so the caller can tell a
    /// full answer from a cut one.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the database fails.
    async fn overrides<C: DBRunner>(
        &self,
        conn: &C,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
        needle: &Needle,
        corpus: Corpus,
        limit: usize,
    ) -> Result<Vec<StoredValue>, DomainError>;

    /// The categories with these ids, for breadcrumbs.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the database fails.
    async fn categories<C: DBRunner>(
        &self,
        conn: &C,
        ids: &[Uuid],
    ) -> Result<Vec<Category>, DomainError>;
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
