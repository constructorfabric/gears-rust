// Created: 2026-09-17 by Virtuozzo International GmbH
//! Cross-field search over the settings hub: what may be matched, and how.
//!
//! Search runs over **stored rows**, never resolved values (DESIGN §4.2
//! *Search*): a declaration's key, description and category name, its Schema
//! Default, and the overrides explicitly set inside the caller's visible
//! subtree. Authorization is applied to the **corpus** before matching, not to
//! the output, because a match's existence, a count and a hit each disclose
//! content on their own. This module holds the vocabulary that rule is stated
//! in; [`service`] applies it and the repository binding runs the queries.

pub mod service;

use std::hash::{DefaultHasher, Hash, Hasher};

use async_trait::async_trait;
use serde_json::Value;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::Category;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::value::StoredValue;
use crate::field;

/// The shortest query the surface accepts: below it every row matches, and
/// a one-character trigram scan is a full scan.
pub const MIN_QUERY_CHARS: usize = 2;

/// The longest query the surface accepts. A substring match is linear in the
/// pattern; a bound keeps a hostile query from turning it into a slow one.
pub const MAX_QUERY_CHARS: usize = 200;

/// A validated search query: trimmed, and within the bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needle(String);

impl Needle {
    /// Trim and bound the raw `q` parameter.
    ///
    /// # Errors
    /// [`DomainError::Validation`] on field `q` when fewer than
    /// [`MIN_QUERY_CHARS`] or more than [`MAX_QUERY_CHARS`] characters remain.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-1
        let trimmed = raw.trim();
        let chars = trimmed.chars().count();
        if chars < MIN_QUERY_CHARS {
            return Err(refused(format!(
                "`q` needs at least {MIN_QUERY_CHARS} characters"
            )));
        }
        if chars > MAX_QUERY_CHARS {
            return Err(refused(format!(
                "`q` may carry at most {MAX_QUERY_CHARS} characters"
            )));
        }
        Ok(Self(trimmed.to_owned()))
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-1
    }

    /// The needle as the caller wrote it, trimmed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `LIKE` pattern: `%needle%` with `%`, `_` and `\` escaped, for a
    /// predicate that declares `ESCAPE '\'`.
    #[must_use]
    pub fn like_pattern(&self) -> String {
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-2
        format!("%{}%", like_escape(&self.0))
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-2
    }

    /// Whether `haystack` contains the needle, case-insensitively — how the
    /// database matched, restated in Rust to name the field that matched.
    #[must_use]
    pub fn matches(&self, haystack: &str) -> bool {
        haystack.to_lowercase().contains(&self.0.to_lowercase())
    }
}

fn refused(message: String) -> DomainError {
    DomainError::Validation {
        field: "q".to_owned(),
        code: field::SEARCH_QUERY,
        message,
    }
}

/// Neutralise `LIKE`'s wildcards and its escape character so the needle is
/// matched literally. The predicate must declare `ESCAPE '\'`.
#[must_use]
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Which stored values the caller may match against. Decided once, before any
/// query runs; `secret` is in neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Corpus {
    /// `public` values only: every caller.
    Public,
    /// `public` and `pii`: a caller holding `read_unmasked`.
    PublicAndPii,
}

impl Corpus {
    /// The corpus a caller's entitlement admits.
    #[must_use]
    pub const fn for_caller(may_read_pii: bool) -> Self {
        if may_read_pii {
            Self::PublicAndPii
        } else {
            Self::Public
        }
    }

    /// The `data_classification` values the corpus admits, for an `IN` list.
    #[must_use]
    pub const fn classifications(self) -> &'static [&'static str] {
        match self {
            Self::Public => &["public"],
            Self::PublicAndPii => &["public", "pii"],
        }
    }

    /// Whether a row of this classification may be matched.
    #[must_use]
    pub fn admits(self, classification: &str) -> bool {
        self.classifications().contains(&classification)
    }
}

/// Which field of a hit matched, in the order a client is told about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchedField {
    Key,
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

/// The token a search's pagination cursor is bound to: the same query, target
/// and corpus mint the same token, and a cursor carrying another is refused.
/// Not a security boundary — the corpus is re-decided on every request — only
/// a guard against continuing one search with another's cursor.
#[must_use]
pub fn cursor_binding(needle: &Needle, tenant: Uuid, corpus: Corpus) -> String {
    let mut hasher = DefaultHasher::new();
    needle.as_str().hash(&mut hasher);
    tenant.hash(&mut hasher);
    corpus.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What a search is asked with, every decision already taken by the caller:
/// the handler decides, the service and the binding only apply.
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
    /// The target and its non-standalone descendants: where an override may
    /// be matched.
    pub tenant_ids: &'a [Uuid],
    /// Page size, cursor and the binding the cursor must carry.
    pub query: &'a ODataQuery,
}

/// The port the search runs through. The binding owns the SQL dialect; the
/// domain states what a query means.
#[async_trait]
pub trait SearchRepository: Send + Sync {
    /// A page of **active** declarations, ordered by key, that match the
    /// request's needle on key, description, the name of their category, their
    /// Schema Default (within the corpus and not JSON `null`), or an override
    /// explicitly set at one of the request's tenants (within the corpus,
    /// never a secret row). The scope and visibility narrow the page as they
    /// narrow browsing; the query carries `limit`, `cursor` and the
    /// `filter_hash` the cursor is bound to.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for a cursor that does not decode or was
    /// minted for another binding; [`DomainError::Internal`] when the database
    /// fails.
    async fn declarations<C: DBRunner>(
        &self,
        conn: &C,
        request: &SearchRequest<'_>,
    ) -> Result<Page<Declaration>, DomainError>;

    /// The overrides of `declaration_ids` set at `tenant_ids` whose text
    /// projection matches `needle`, within `corpus`; never a secret row and
    /// never a subject-scoped row. Ordered by declaration, then tenant.
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
