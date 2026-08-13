// Created: 2026-08-12 by Constructor Tech
//! GTS resource types this gear attributes its errors to.
//!
//! These are the values that reach `NotFound.ctx.resource_type` and its
//! siblings. They are the discriminator the [`crate::SettingsError`] projection
//! uses to tell two otherwise identical `NotFound` outcomes apart: a setting
//! that was never declared, and a secret-backed setting whose credential is not
//! configured at any scope. A consumer that conflates the two hands a
//! placeholder to its backend believing it to be a credential.

/// A setting declaration — the record of what a setting *is*.
pub const DECLARATION_SCHEMA: &str = "gts.cf.toolkit.settings.declaration.v1~";

/// A stored setting value at some scope — what a setting currently *holds*.
pub const VALUE_SCHEMA: &str = "gts.cf.toolkit.settings.value.v1~";

/// A settings category.
pub const CATEGORY_SCHEMA: &str = "gts.cf.toolkit.settings.category.v1~";

/// Typed view of the wire `resource_type` strings above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// [`DECLARATION_SCHEMA`]
    Declaration,
    /// [`VALUE_SCHEMA`]
    Value,
    /// [`CATEGORY_SCHEMA`]
    Category,
    /// A resource type this SDK does not model, preserved verbatim.
    Unknown(String),
}

impl Resource {
    /// Read the discriminator from a wire `resource_type` string.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            DECLARATION_SCHEMA => Self::Declaration,
            VALUE_SCHEMA => Self::Value,
            CATEGORY_SCHEMA => Self::Category,
            // Preserved rather than discarded: a consumer can still report an
            // unmodelled resource, and a later version can model it without a
            // migration.
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire string.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Declaration => DECLARATION_SCHEMA,
            Self::Value => VALUE_SCHEMA,
            Self::Category => CATEGORY_SCHEMA,
            Self::Unknown(raw) => raw,
        }
    }
}

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
