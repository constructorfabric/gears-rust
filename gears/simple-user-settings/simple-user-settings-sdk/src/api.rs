//! `SimpleUserSettingsClientV1` trait definition.
//!
//! This trait defines the public API for the settings gear (Version 1).
//! All methods require a `SecurityContext` for authorization and access control.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{
    NamedSetting, SimpleUserSettings, SimpleUserSettingsPatch, SimpleUserSettingsUpdate,
};

/// Public API trait for the settings gear (Version 1).
///
/// This trait is registered in `ClientHub` by the settings gear:
/// ```ignore
/// let settings = hub.get::<dyn SimpleUserSettingsClientV1>()?;
/// ```
///
/// All methods require a `SecurityContext` for proper authorization and
/// access control. Errors are returned as the platform's canonical type
/// (`CanonicalError`) per ADR 0005 — consumers either propagate via `?`
/// or match on canonical categories directly. This SDK ships no typed
/// projection because its small surface (three CRUD-style methods with
/// validation and not-found dispositions) does not warrant one.
#[async_trait]
pub trait SimpleUserSettingsClientV1: Send + Sync {
    /// Get settings for the current user.
    /// Returns default empty values if no settings record exists.
    async fn get_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<SimpleUserSettings, CanonicalError>;

    /// Update settings with full replacement (POST semantics).
    /// Creates a new record if none exists.
    async fn update_settings(
        &self,
        ctx: &SecurityContext,
        update: SimpleUserSettingsUpdate,
    ) -> Result<SimpleUserSettings, CanonicalError>;

    /// Partially update settings (PATCH semantics).
    /// Only updates provided fields. Creates a new record if none exists.
    async fn patch_settings(
        &self,
        ctx: &SecurityContext,
        patch: SimpleUserSettingsPatch,
    ) -> Result<SimpleUserSettings, CanonicalError>;
}

/// Named settings: any number of keyed JSON values per user, next to the fixed
/// `theme` and `language` of [`SimpleUserSettingsClientV1`].
///
/// A separate trait rather than new methods on `SimpleUserSettingsClientV1`, so
/// existing implementations of that trait keep compiling. The settings gear
/// registers both in `ClientHub`:
/// ```ignore
/// let named = hub.get::<dyn NamedSettingsClientV1>()?;
/// named.put_named_setting(&ctx, "portal.projects.view", json!("table")).await?;
/// ```
///
/// Settings are filed under the caller's `(user, tenant)` and authorized as the
/// same resource as the fixed fields: reads need `get`, writes and deletes need
/// `update`.
#[async_trait]
pub trait NamedSettingsClientV1: Send + Sync {
    /// Every named setting the caller has, ordered by key.
    async fn list_named_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<NamedSetting>, CanonicalError>;

    /// One named setting, or `None` if the caller has not set it.
    async fn get_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<Option<NamedSetting>, CanonicalError>;

    /// Create or replace one named setting.
    ///
    /// Fails with `InvalidArgument` for a malformed key or a value over the size
    /// bound, and with `ResourceExhausted` (quota code `NAMED_SETTINGS_PER_USER`)
    /// for a new key past the per-user count bound: free room by deleting one.
    async fn put_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
        value: serde_json::Value,
    ) -> Result<NamedSetting, CanonicalError>;

    /// Forget one named setting. Returns whether it existed; deleting a key
    /// that is not set is not an error.
    async fn delete_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<bool, CanonicalError>;

    /// Forget every named setting the caller has, in one call. Returns how many
    /// were removed; with none set it removes nothing and is not an error.
    async fn delete_all_named_settings(&self, ctx: &SecurityContext)
    -> Result<u64, CanonicalError>;
}
