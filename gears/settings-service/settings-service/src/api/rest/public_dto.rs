//! Wire shapes of the anonymous read surface.
//!
//! Deliberately minimal: a key and its effective value. Nothing that
//! describes the declaration (its classification, scope, source or trail) is
//! served, so the response reveals no more than the flag was set to publish.

use serde_json::Value;

/// One exposable setting at the requested tenant.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct PublicSettingDto {
    /// The setting's GTS key.
    pub key: String,
    /// Its effective value at the requested tenant.
    pub value: Value,
}

/// The anonymous read's answer: the exposable settings, in key order.
///
/// Not paged: the set is the declarations an administrator flagged, a short
/// reviewed list, not a table that grows with tenants.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct PublicSettingsDto {
    /// The exposable settings with their values; empty for a tenant that has
    /// none, for one that does not exist and for an unusable `tenant`.
    pub items: Vec<PublicSettingDto>,
}

impl PublicSettingsDto {
    /// The answer of an unknown, missing or malformed tenant: the same as a
    /// tenant with nothing to show.
    #[must_use]
    pub const fn empty() -> Self {
        Self { items: Vec::new() }
    }
}
