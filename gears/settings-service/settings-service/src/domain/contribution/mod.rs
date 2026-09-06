// Created: 2026-09-06 by Constructor Tech
//! Module-contributed declarations: the reconciler behind the SDK's
//! `SettingsContributionClient`.
//!
//! Gears register their declarations from their own init on every boot, so the
//! reconcile is idempotent and never fails a whole set for one bad item: each
//! declaration is reconciled on its own, and a refused one is reported per key.

pub mod service;

use async_trait::async_trait;
use settings_service_sdk::SettingKey;

use crate::domain::error::DomainError;

pub use service::{ContributionService, ItemError, Outcome};

/// Stable reasons a contributed declaration is refused with.
///
/// Callers match on these, never on the message.
pub mod reason {
    /// The key's derived half carries no category segment or is malformed.
    pub const KEY_NOT_NAMESPACED: &str = "key_not_namespaced";
    /// The named value type is not registered.
    pub const VALUE_TYPE_UNKNOWN: &str = "value_type_unknown";
    /// The Schema Default fails its value type.
    pub const DEFAULT_INVALID: &str = "default_invalid";
    /// A secret-trait type was given a non-empty default.
    pub const SECRET_DEFAULT_NOT_EMPTY: &str = "secret_default_not_empty";
    /// The caller's classification contradicts the value type's trait.
    pub const CLASSIFICATION_CONFLICT: &str = "classification_conflict";
    /// `anonymous_exposable` was asked for on a `secret` or `pii` setting.
    pub const EXPOSABLE_NOT_SENSITIVE: &str = "exposable_not_sensitive";
    /// The value type changed at the same major.
    pub const VALUE_TYPE_CHANGED: &str = "value_type_changed";
    /// The Schema Default or the scope class changed at the same major.
    pub const BEHAVIOR_AFFECTING_CHANGE: &str = "behavior_affecting_change";
    /// A lower major than the active one was registered.
    pub const MAJOR_REGRESSION: &str = "major_regression";
    /// A higher major arrived; the upgrade migration is not built yet.
    pub const UPGRADE_UNSUPPORTED: &str = "upgrade_unsupported";
    /// The key belongs to another module.
    pub const NOT_OWNER: &str = "not_owner";
    /// No declaration exists at the key.
    pub const NOT_FOUND: &str = "not_found";
}

/// Registers a setting's own type in the types registry before its row exists.
///
/// A port because the composition of the derived schema and the call to the
/// registry are infrastructure; the domain states only that the type is
/// registered first and that registration is idempotent.
#[async_trait]
pub trait SettingTypeRegistrar: Send + Sync {
    /// Register the type identified by `key`, derived from the abstract
    /// `setting_type` base and narrowed to `value_type_id`.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached or the
    /// base is absent; [`DomainError::Internal`] on any other refusal. An
    /// already-registered type is success.
    async fn register_setting_type(
        &self,
        key: &SettingKey,
        value_type_id: &str,
    ) -> Result<(), DomainError>;
}
