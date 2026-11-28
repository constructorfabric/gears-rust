//! Validated `OAuth2` scope sets.
//!
//! A [`ScopeSet`] holds RFC 6749 section 3.3 scope-tokens: case-sensitive,
//! each one or more bytes `0x21`, `0x23..=0x5B` or `0x5D..=0x7E`. Input is not
//! trimmed; blank or whitespace-containing items are rejected. Order and
//! duplicates are immaterial, so the canonical form is sorted and deduplicated.

use std::fmt;
use std::sync::Arc;

/// A non-empty, validated set of `OAuth2` scopes in canonical form.
///
/// Equal sets compare and hash equal regardless of input order and
/// duplicates; a subset or superset is a different set. [`fmt::Display`]
/// yields the space-separated `scope` request parameter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeSet(Arc<[String]>);

/// Why scopes were rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeSetError {
    /// No scopes.
    #[error("scope set is empty")]
    Empty,
    /// More than [`ScopeSet::MAX_SCOPES`] items, counting duplicates.
    #[error("more than {} scopes", ScopeSet::MAX_SCOPES)]
    TooMany,
    /// The item at `index` exceeds [`ScopeSet::MAX_SCOPE_LEN`] bytes.
    #[error("scope at index {index} exceeds {} bytes", ScopeSet::MAX_SCOPE_LEN)]
    TokenTooLong {
        /// Position in the input.
        index: usize,
    },
    /// Items exceed [`ScopeSet::MAX_TOTAL_LEN`] bytes in total.
    #[error("scopes exceed {} bytes in total", ScopeSet::MAX_TOTAL_LEN)]
    TooLarge,
    /// The item at `index` is not an RFC 6749 scope-token.
    #[error("scope at index {index} is not a valid scope token")]
    InvalidToken {
        /// Position in the input.
        index: usize,
    },
}

impl ScopeSet {
    /// Maximum number of input items, duplicates included.
    pub const MAX_SCOPES: usize = 64;
    /// Maximum length of one scope, in bytes.
    pub const MAX_SCOPE_LEN: usize = 256;
    /// Maximum total length of all input items, in bytes.
    pub const MAX_TOTAL_LEN: usize = 4096;

    /// Validate and canonicalize `scopes`.
    ///
    /// # Errors
    ///
    /// [`ScopeSetError`] for the first violation.
    pub fn new<I>(scopes: I) -> Result<Self, ScopeSetError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        Self::optional(scopes)?.ok_or(ScopeSetError::Empty)
    }

    /// Like [`Self::new`], but no scopes yield `None`.
    ///
    /// # Errors
    ///
    /// [`ScopeSetError`] for the first violation.
    pub fn optional<I>(scopes: I) -> Result<Option<Self>, ScopeSetError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut items = Vec::new();
        let mut total = 0usize;
        // Bounds are enforced per item, so oversized input is rejected
        // before it is collected or sorted.
        for (index, scope) in scopes.into_iter().enumerate() {
            if index >= Self::MAX_SCOPES {
                return Err(ScopeSetError::TooMany);
            }
            let scope = scope.as_ref();
            if scope.len() > Self::MAX_SCOPE_LEN {
                return Err(ScopeSetError::TokenTooLong { index });
            }
            if scope.is_empty() || !scope.bytes().all(is_scope_token_byte) {
                return Err(ScopeSetError::InvalidToken { index });
            }
            total += scope.len();
            if total > Self::MAX_TOTAL_LEN {
                return Err(ScopeSetError::TooLarge);
            }
            items.push(scope.to_owned());
        }
        if items.is_empty() {
            return Ok(None);
        }
        items.sort_unstable();
        items.dedup();
        Ok(Some(Self(items.into())))
    }

    /// Scopes in canonical order; never empty.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Iterate scopes in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl fmt::Display for ScopeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut scopes = self.iter();
        if let Some(first) = scopes.next() {
            f.write_str(first)?;
        }
        for scope in scopes {
            f.write_str(" ")?;
            f.write_str(scope)?;
        }
        Ok(())
    }
}

/// RFC 6749 section 3.3: `%x21 / %x23-5B / %x5D-7E`.
const fn is_scope_token_byte(byte: u8) -> bool {
    matches!(byte, 0x21 | 0x23..=0x5B | 0x5D..=0x7E)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "scopes_tests.rs"]
mod scopes_tests;
