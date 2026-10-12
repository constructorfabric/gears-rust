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

/// One named setting: a key the product chose, and a JSON value.
///
/// Named settings sit next to the fixed `theme` and `language` and are filed
/// under the same `(user, tenant)`, so they carry neither: whose they are is
/// decided by the caller's security context, exactly as for the fixed fields.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedSetting {
    /// 1–128 characters from `A–Z a–z 0–9 . _ - :`. Namespacing is the
    /// product's business; a dotted prefix (`portal.projects.view`) is the
    /// usual shape.
    pub key: String,
    /// Any JSON value, bounded in size by the gear's configuration.
    pub value: serde_json::Value,
}
