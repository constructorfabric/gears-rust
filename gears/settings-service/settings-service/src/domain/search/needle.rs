//! The vocabulary of a text match: the validated query and the corpus it
//! may reach.
//!
//! A leaf: the search and the browse narrowing both use it, and it imports
//! nothing of either, so neither depends on the other through it.

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::field;

/// The shortest query the surface accepts: below it every row matches, and
/// a one-character trigram scan is a full scan.
pub const MIN_QUERY_CHARS: usize = 2;

/// The longest query the surface accepts. A substring match is linear in the
/// pattern; a bound keeps a hostile query from turning it into a slow one.
pub const MAX_QUERY_CHARS: usize = 200;

/// A validated search query: trimmed, and within the bounds.
#[domain_model]
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
#[domain_model]
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
