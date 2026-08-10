//! `TenantIdpMetadataV1` projection codec.
//!
//! Plugin-private JSON shape persisted by AM in `tenant_idp_metadata.metadata`
//! and replayed on every subsequent `IdP` call for the tenant. AM never
//! inspects it; the plugin is the sole writer.
//!
//! See DESIGN §4.1 (entity), §4.5 (codec rules), §5.3 (output contract).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Plugin-private metadata blob. Version-tagged for forward compatibility.
// The derives are this module's codec — see the `//!` docs.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantIdpMetadataV1 {
    /// Schema version tag. Always `"v1"` at this revision.
    pub version: String,
    /// Keycloak realm hosting the tenant's users and groups.
    pub realm_name: String,
    /// How the realm came to be bound (Shared / Adopted / Created).
    pub realm_binding: RealmBinding,
    /// Keycloak-issued group identifier returned by `POST .../groups` at
    /// provision time. Cached because Keycloak's group API is keyed by id.
    pub tenant_group_id: Uuid,
    /// `SecretRef` ID of the per-realm admin client's secret in `CredStore`.
    ///
    /// Per ADDENDUM §3.4: `None` for tenants in the default shared realm
    /// (env-backed); `Some(_)` for Adopted/Created bindings (OpenBao-backed).
    #[serde(default)]
    pub admin_secret_ref: Option<String>,
    /// `OAuth2` `client_id` of the per-realm admin client inside `realm_name`.
    pub admin_client_id: String,
}

/// Realm-binding mode. Same enum is the input discriminator on
/// `provisioning_metadata` and the persisted state on `TenantIdpMetadataV1`
/// (DESIGN §4.1 enum row).
// Serializes for both roles described above.
#[allow(unknown_lints, de0101_no_serde_in_contract)]
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealmBinding {
    Shared,
    Adopted,
    Created,
}

impl RealmBinding {
    /// Stable `realm_binding` metric-label string. The literals are part
    /// of the public metric contract — every dashboard / alert keyed on
    /// `realm_binding` reads these values verbatim. Returning
    /// `&'static str` keeps the value attachable as an `OTel`
    /// `KeyValue` without per-call allocation.
    #[must_use]
    pub const fn as_metric_label(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Adopted => "adopted",
            Self::Created => "created",
        }
    }
}

#[domain_model]
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("metadata blob has no 'version' key")]
    MissingVersion,
    #[error("unsupported metadata version: {observed}")]
    UnsupportedVersion { observed: String },
    #[error("malformed metadata blob: {detail}")]
    Malformed { detail: String },
}

/// Stable metric-label form of a [`DecodeError`] variant, used by the
/// `keycloak_idp_plugin_metadata_decode_failure_total` counter's `version_observed`
/// label. Centralised so the three SDK-facing decode paths cannot drift.
///
/// Returns `Cow::Borrowed` for the two static variants to avoid allocation on
/// the common `MissingVersion` and `Malformed` paths.
#[must_use]
pub fn version_observed_label(e: &DecodeError) -> std::borrow::Cow<'static, str> {
    match e {
        DecodeError::MissingVersion => std::borrow::Cow::Borrowed("missing"),
        DecodeError::UnsupportedVersion { observed } => std::borrow::Cow::Owned(observed.clone()),
        DecodeError::Malformed { .. } => std::borrow::Cow::Borrowed("malformed"),
    }
}

/// Stateless codec.
#[domain_model]
pub struct MetadataCodec;

impl MetadataCodec {
    /// Serialise to JSON. Infallible — the struct is always serialisable.
    ///
    /// # Panics
    ///
    /// Panics only if `serde_json::to_value` fails, which is impossible for
    /// `TenantIdpMetadataV1` (no `Map`-incompatible keys, no custom
    /// `Serialize` impl that can error). The `expect` anchors that invariant.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "stateless codec is exposed via method-style API for symmetry with potential future state"
    )]
    #[expect(
        clippy::expect_used,
        reason = "TenantIdpMetadataV1 has no serialiser-defeating shape; the expect anchors that invariant"
    )]
    pub fn encode(&self, v: &TenantIdpMetadataV1) -> serde_json::Value {
        serde_json::to_value(v).expect("TenantIdpMetadataV1 is always serialisable")
    }

    /// Decode from `Option<serde_json::Value>`. `None` → `None`. `Some(_)`:
    /// reject unknown versions and shape mismatches.
    ///
    /// # Errors
    ///
    /// - [`DecodeError::MissingVersion`] — blob has no `version` key.
    /// - [`DecodeError::UnsupportedVersion`] — blob's `version` is not `"v1"`.
    /// - [`DecodeError::Malformed`] — blob is `v1` but does not match the
    ///   [`TenantIdpMetadataV1`] schema.
    #[expect(
        clippy::unused_self,
        reason = "stateless codec is exposed via method-style API for symmetry with potential future state"
    )]
    pub fn decode(
        &self,
        blob: Option<serde_json::Value>,
    ) -> Result<Option<TenantIdpMetadataV1>, DecodeError> {
        let Some(value) = blob else {
            return Ok(None);
        };
        let version = value
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or(DecodeError::MissingVersion)?;
        if version != "v1" {
            return Err(DecodeError::UnsupportedVersion {
                observed: version.into(),
            });
        }
        serde_json::from_value::<TenantIdpMetadataV1>(value)
            .map(Some)
            .map_err(|e| DecodeError::Malformed {
                detail: e.to_string(),
            })
    }

    /// Quick check for AM-replay paths that want to detect blobs produced by
    /// older plugin instances.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "stateless codec is exposed via method-style API for symmetry with potential future state"
    )]
    pub fn is_v1_or_compatible(&self, v: &serde_json::Value) -> bool {
        v.get("version")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s == "v1")
    }
}

#[cfg(test)]
#[path = "metadata_codec_tests.rs"]
mod metadata_codec_tests;
