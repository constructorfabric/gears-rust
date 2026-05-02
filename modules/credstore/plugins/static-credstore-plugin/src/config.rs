// Updated: 2026-04-07 by Constructor Tech
use serde::Deserialize;
use uuid::Uuid;

use credstore_sdk::SharingMode;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize, modkit_macros::ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct StaticCredStorePluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Static secrets served by this plugin.
    #[expand_vars]
    pub secrets: Vec<SecretConfig>,
}

impl Default for StaticCredStorePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            priority: 100,
            secrets: Vec::new(),
        }
    }
}

/// A single secret entry in the plugin configuration.
#[derive(Clone, Deserialize, modkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct SecretConfig {
    /// Tenant that owns this secret.
    ///
    /// - `None` → **global** secret, accessible by any tenant (uses
    ///   `SharingMode::Shared` on the wire but stored in a separate
    ///   global map in the static plugin).
    /// - `Some` with `SharingMode::Shared` → **shared** secret scoped to
    ///   this tenant, visible to descendants via gateway hierarchy walk-up.
    /// - `Some` with `SharingMode::Tenant` → **tenant** secret, visible
    ///   only within this tenant.
    ///
    /// `owner_id` cannot be set without `tenant_id`.
    pub tenant_id: Option<Uuid>,

    /// Owner (subject) of this secret.
    ///
    /// **Only valid for `Private` sharing mode.** When set, the secret is
    /// keyed by `(tenant_id, owner_id, key)` and matched against
    /// `SecurityContext::subject_id()` at lookup time.
    ///
    /// Requires `tenant_id` to be set. Rejected at init if the resolved
    /// sharing mode is not `Private`.
    ///
    /// For `Tenant`/`Shared`/global secrets, `owner_id` must be `None`;
    /// the returned `SecretMetadata::owner_id` is filled from
    /// `SecurityContext::subject_id()` of the caller.
    pub owner_id: Option<Uuid>,

    /// Secret reference key (validated as `SecretRef` at init).
    pub key: String,

    /// Secret value (plaintext string, converted to bytes at init).
    #[expand_vars]
    pub value: String,

    /// Sharing mode for this secret.
    /// When `None`, inferred from `tenant_id`/`owner_id`:
    /// - `tenant_id=None` → `Shared`
    /// - `tenant_id=Some`, `owner_id=None` → `Tenant`
    /// - `tenant_id=Some`, `owner_id=Some` → `Private`
    pub sharing: Option<SharingMode>,
}

impl SecretConfig {
    /// Resolve the effective sharing mode from the explicit value or the
    /// `tenant_id`/`owner_id` combination.
    #[must_use]
    pub fn resolve_sharing(&self) -> SharingMode {
        self.sharing
            .unwrap_or(match (self.tenant_id, self.owner_id) {
                (None, _) => SharingMode::Shared,
                (Some(_), None) => SharingMode::Tenant,
                (Some(_), Some(_)) => SharingMode::Private,
            })
    }
}

impl core::fmt::Debug for SecretConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecretConfig")
            .field("tenant_id", &self.tenant_id)
            .field("owner_id", &self.owner_id)
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .field("sharing", &self.resolve_sharing())
            .finish()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
