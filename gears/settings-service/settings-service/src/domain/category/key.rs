// Created: 2026-08-13 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-category-management-key-format:p1
//! The category key value object.
//!
//! A category key is not a display label. It becomes the **category segment of
//! every setting key declared under it** — the `network` in
//! `acme.settings.network.enable_proxy.v1` — so its shape is constrained by the
//! GTS instance-id grammar rather than by presentation.
//!
//! That is why `/` is rejected. A key carrying a separator would suggest nesting
//! the grammar cannot express: a setting key has exactly one category segment,
//! and categories are flat (ADR-001, and the PRD's own exclusion of nesting).

use crate::field;

/// Inclusive bounds on a category key's length, in characters.
const MIN_LENGTH: usize = 1;
const MAX_LENGTH: usize = 128;

/// The separator reserved by the setting-key grammar.
const RESERVED_SEPARATOR: char = '/';

/// A validated category key.
///
/// Holds the candidate verbatim: parsing never trims or case-folds, so a stored
/// key and a supplied key compare identically. Accepting `" network"` as
/// `network` would give one category two spellings, and the setting keys
/// declared under each would not match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CategoryKey(String);

impl CategoryKey {
    /// Validate a candidate category key.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`](crate::domain::error::DomainError::Validation)
    /// naming the violated rule — the length bound, or the reserved separator.
    pub fn parse(candidate: &str) -> Result<Self, crate::domain::error::DomainError> {
        use crate::domain::error::DomainError;

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-1
        // Verbatim: no trim, no case-fold. Every check below reads the candidate
        // exactly as supplied, so what is validated is what is stored.
        let length = candidate.chars().count();
        // @cpt-end:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-1

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-2
        // Counted in characters, not bytes: the bound is a limit on the name an
        // administrator writes, and a multi-byte key would otherwise be rejected
        // for a length its author never sees.
        if !(MIN_LENGTH..=MAX_LENGTH).contains(&length) {
            return Err(DomainError::Validation {
                field: FIELD.to_owned(),
                code: field::CATEGORY_KEY_LENGTH,
                message: format!(
                    "a category key is {MIN_LENGTH} to {MAX_LENGTH} characters; this one is {length}"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-2

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-3
        if candidate.contains(RESERVED_SEPARATOR) {
            return Err(DomainError::Validation {
                field: FIELD.to_owned(),
                code: field::CATEGORY_KEY_RESERVED_SEPARATOR,
                message: format!(
                    "`{RESERVED_SEPARATOR}` is reserved: the category key becomes the single \
                     category segment of every setting key declared under it, and categories \
                     are flat"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-3

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-4
        Ok(Self(candidate.to_owned()))
        // @cpt-end:cpt-cf-settings-service-algo-category-management-key-validation:p1:inst-cat-keyval-4
    }

    /// The key as stored and compared.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CategoryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The request field a key violation is reported against.
pub const FIELD: &str = "key";

#[cfg(test)]
#[path = "key_tests.rs"]
mod key_tests;
