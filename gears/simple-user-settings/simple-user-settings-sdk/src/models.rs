//! Public models for the settings gear.
//!
//! These are transport-agnostic data structures that define the contract
//! between the settings gear and its consumers.
//!
//! All models are marked with `#[domain_model]` to enforce DDD boundaries
//! at compile time - they cannot contain infrastructure types.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// User settings entity.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleUserSettings {
    /// The user half of the key these settings are filed under: the caller's
    /// token subject, or what the deployment's
    /// [`SettingsOwnerResolver`](crate::SettingsOwnerResolver) resolved it to.
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub theme: Option<String>,
    pub language: Option<String>,
}

/// Partial update data for user settings.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimpleUserSettingsPatch {
    pub theme: Option<String>,
    pub language: Option<String>,
}

/// Full update data for user settings.
///
/// Unlike `SimpleUserSettingsPatch`, all fields are required and represent a full replacement.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleUserSettingsUpdate {
    pub theme: String,
    pub language: String,
}
