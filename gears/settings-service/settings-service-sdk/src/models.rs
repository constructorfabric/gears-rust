// Created: 2026-08-11 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-sdk-models:p1
//! Public models exchanged with settings consumers.
//!
//! The opaque secret handle and the effective-source vocabulary, the reader
//! request and response shapes, and the declaration-contribution shapes.
//!
//! The change-notification payloads belong to Settings Activation and are not
//! modelled here; see the [`crate::api`] module docs for why.

use serde::{Deserialize, Serialize};

use crate::SettingKey;

/// Where an effective value resolved from.
///
/// A successful read always carries a value, because every declaration has a
/// Schema Default and all three scope-class algorithms terminate in one. A
/// consumer distinguishing *an administrator set this* from *nobody has touched
/// it* therefore reads the source, never the value: a setting whose type admits
/// `null` may legitimately be set to `null`, which is indistinguishable by
/// inspection from a `null` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveSource {
    /// An override exists at the requested scope.
    OwnOverride,
    /// Resolved from a nearest-ancestor override.
    Inherited,
    /// No override anywhere in the chain; the declaration's own default.
    SchemaDefault,
}

impl EffectiveSource {
    /// Whether this source means no override exists anywhere in the chain.
    ///
    /// For a `secret`-trait setting this is also how a machine consumer detects
    /// an unconfigured credential: the declaration's default is a non-secret
    /// placeholder, so a schema-default source means no credential is set at any
    /// scope and the placeholder must be treated as absent.
    #[must_use]
    pub const fn is_unconfigured(self) -> bool {
        matches!(self, Self::SchemaDefault)
    }
}

/// An opaque reference to a `secret`-trait value.
///
/// Returned in place of plaintext on every read of a secret-backed setting.
/// The handle deliberately carries **no** Credential Store coordinates, so a
/// consumer cannot bypass the audited resolution path by reading it apart; the
/// only way to obtain plaintext is to present the handle back to the reader,
/// which authorizes the caller against that specific setting and emits a
/// secret-use audit event.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretHandle(String);

impl SecretHandle {
    /// Wrap an opaque token as a secret handle.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The opaque token, for transport only.
    #[must_use]
    pub fn as_token(&self) -> &str {
        &self.0
    }
}

/// Redacted on purpose: a secret handle must never widen a log line into a
/// disclosure path, so neither the token nor any derived material is printed.
impl std::fmt::Debug for SecretHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretHandle(<redacted>)")
    }
}

/// A request for one setting's effective value at a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetEffectiveRequest {
    /// The setting to resolve.
    pub key: SettingKey,
    /// The scope to resolve it for.
    pub scope: String,
}

/// A resolved effective value with the trace of where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveValueResponse {
    /// The setting that was resolved.
    pub key: SettingKey,
    /// The scope it was resolved for.
    pub scope: String,
    /// The resolved value. A secret-backed setting carries its masked handle.
    pub value: serde_json::Value,
    /// Where the value came from. Read this, not the value, to tell a
    /// configured setting from an untouched one.
    pub source: EffectiveSource,
    /// The scope that supplied the value; absent for a Schema Default.
    pub source_scope: Option<String>,
}

/// One declaration a module contributes at install or upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContributedDeclaration {
    /// The setting key the module supplies.
    pub key: SettingKey,
    /// The Schema Default, validated against the key's value type.
    pub default_value: serde_json::Value,
}

/// Outcome of one reconcile pass over a module's declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    /// Declarations newly inserted.
    pub registered: usize,
    /// Declarations updated in place.
    pub updated: usize,
    /// Declarations moved to retired.
    pub retired: usize,
    /// Declarations revived from retired.
    pub reactivated: usize,
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
