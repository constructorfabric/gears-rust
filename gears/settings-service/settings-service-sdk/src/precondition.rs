// Created: 2026-08-12 by Constructor Tech
//! Precondition-violation vocabulary.
//!
//! A retired setting is not an absent one: the declaration row exists and
//! carries `status = retired`. That is a positive fact, so it is reported as a
//! failed precondition rather than as a not-found — the consumer should drop the
//! dependency rather than retry or wait for it to appear.
//!
//! The platform canonical vocabulary has no `Retired` category, so the
//! distinction travels as the violation `type_` string below and is fanned back
//! into a typed variant by [`crate::SettingsError`].

/// The setting's declaration was withdrawn, so it no longer resolves.
///
/// A retry will never succeed. The consumer should stop reading the key.
pub const SETTING_RETIRED: &str = "SETTING_RETIRED";

/// Typed view of the wire precondition `type_` strings above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreconditionKind {
    /// [`SETTING_RETIRED`]
    SettingRetired,
    /// A precondition type this SDK does not model, preserved verbatim.
    Unknown(String),
}

impl PreconditionKind {
    /// Read the discriminator from a wire violation `type_` string.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            SETTING_RETIRED => Self::SettingRetired,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire string.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::SettingRetired => SETTING_RETIRED,
            Self::Unknown(raw) => raw,
        }
    }
}

#[cfg(test)]
#[path = "precondition_tests.rs"]
mod precondition_tests;
