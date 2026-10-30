//! The closed vocabularies a declaration carries as strings in storage and on
//! the wire: its data classification and its lifecycle status.
//!
//! The stored fields stay strings (the database holds them under CHECK
//! constraints and the wire shapes name them); these enums are what the domain
//! compares, so a typo is a compile error and not a branch that never fires.

use std::str::FromStr;

use toolkit_macros::domain_model;

/// How sensitive a declaration's values are.
///
/// The derive order is the strictness order: `Public < Pii < Secret`. The
/// strictest of several classes is therefore their `max`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataClassification {
    /// Shown to any reader.
    Public,
    /// Masked for a caller not authorized for unmasked PII.
    Pii,
    /// Never shown.
    Secret,
}

impl DataClassification {
    /// The wire and storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Pii => "pii",
            Self::Secret => "secret",
        }
    }

    /// The one way a *stored* string becomes the enum. A string that is none
    /// of the three ranks as `Secret` — fail closed, so an unknown class masks
    /// rather than shows.
    #[must_use]
    pub fn from_stored(stored: &str) -> Self {
        stored.parse().unwrap_or(Self::Secret)
    }
}

/// A string that names no classification.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKind;

impl FromStr for DataClassification {
    type Err = UnknownKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public" => Ok(Self::Public),
            "pii" => Ok(Self::Pii),
            "secret" => Ok(Self::Secret),
            _ => Err(UnknownKind),
        }
    }
}

/// Where a declaration stands in its lifecycle.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclarationStatus {
    /// Resolves and accepts writes.
    Active,
    /// Excluded from resolution; its values are kept for a revive.
    Retired,
}

impl DeclarationStatus {
    /// The wire and storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }

    /// Whether the declaration is active.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    /// Whether the declaration is retired.
    #[must_use]
    pub const fn is_retired(self) -> bool {
        matches!(self, Self::Retired)
    }

    /// The one way a *stored* string becomes the enum. A string that is
    /// neither status counts as `Retired` — fail closed, so an unknown status
    /// never resolves.
    #[must_use]
    pub fn from_stored(stored: &str) -> Self {
        stored.parse().unwrap_or(Self::Retired)
    }
}

impl FromStr for DeclarationStatus {
    type Err = UnknownKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            _ => Err(UnknownKind),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "kinds_tests.rs"]
mod kinds_tests;
