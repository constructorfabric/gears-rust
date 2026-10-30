// Updated: 2026-10-06 by Constructor Tech
//! GTS-based secret types and their enforceable traits.
//!
//! A *secret type* classifies a credential and binds the handling rules the
//! gear enforces uniformly — most importantly which [`SharingMode`]s the
//! type permits. Every type is a GTS type derived from the credstore
//! credential base type ([`crate::CREDENTIAL_RESOURCE_TYPE`]), renamed from
//! `secret.v1~` to `credential.v1~` by ADR-0004, Naming:
//!
//! ```text
//! gts.cf.core.credstore.credential.v1~cf.core.credstore.<name>.v1~
//! ```
//!
//! The types registry is the source of truth for types at run time; the
//! catalog below only seeds the built-in types. Each entry's GTS schema is
//! registered with the types-registry via the link-time inventory (see
//! [`crate::gts`]), which makes the types discoverable and per-type PDP
//! targeting possible without new machinery. Adding a built-in type means
//! adding a catalog entry (and its schema struct); the enforcement code is
//! trait-driven and needs no change unless a new *trait* is introduced.
//!
//! The type of a secret is named at creation (a create without a type is
//! rejected) and is immutable for the secret's lifetime.

use std::fmt;

use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::models::SharingMode;

/// Enforceable traits of a secret type.
///
/// All fields are enforced by the gear on write except
/// `rotation_period_secs` (advisory) and `expirable`, which additionally
/// gates reads (the secret of an expired record is never served; the record
/// itself stays visible with status `expired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretTypeDescriptor {
    /// Catalog name of a built-in type (e.g. `"api-key"`).
    pub name: &'static str,
    /// Full derived GTS type id (the gear stores its v5 UUID).
    pub gts_id: &'static str,
    /// Sharing modes permitted for secrets of this type.
    pub allow_sharing: &'static [SharingMode],
    /// Embedded JSON Schema the (UTF-8, JSON) value must satisfy, if any.
    pub value_schema: Option<&'static str>,
    /// Upper bound on the raw value size. Every type has a finite limit;
    /// there is no platform default to fall back on.
    pub max_size_bytes: usize,
    /// Whether secrets of this type may carry `expires_at`; the secret of an
    /// expired record is never served (`SecretExpired`, evaluated from the
    /// row at read time).
    pub expirable: bool,
    /// Advisory rotation cadence; surfaced via metadata only.
    pub rotation_period_secs: Option<u64>,
    /// Whether the value must be valid UTF-8 (all current types; a future
    /// binary type would clear this).
    pub utf8_only: bool,
}

/// Trait schema (`x-gts-traits-schema`) carried by the secret base type,
/// and the runtime carrier of a secret type's enforceable traits.
///
/// Derived secret-type schemas (see `crate::gts`, and any registered later)
/// declare their `x-gts-traits` values against this shape; the registry
/// validates them at registration (`deny_unknown_fields` ⇒
/// `additionalProperties: false`, so no stray trait keys). At operation time
/// the gear resolves a type's effective traits (chain-merged, leaf wins) from
/// the types-registry and deserializes them into this struct — the registry is
/// the source of truth for enforcement; [`SECRET_TYPE_CATALOG`] only seeds the
/// built-in schemas.
///
/// `allow_sharing` and `max_size_bytes` are **required with no default**: the
/// base type supplies no trait values, so every non-abstract type must declare
/// both, and the registry refuses (at registration) a type that leaves either
/// unset. The remaining traits are optional and fall back to their schema
/// defaults.
#[derive(
    Debug,
    Clone,
    PartialEq,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
    gts_macros::GtsTraitsSchema,
)]
#[serde(deny_unknown_fields)]
pub struct SecretTypeTraits {
    /// Sharing modes permitted for secrets of this type.
    #[schemars(schema_with = "sharing_modes_schema")]
    pub allow_sharing: Vec<SharingMode>,
    /// Whether secrets of this type may carry an expiry.
    #[serde(default)]
    pub expirable: bool,
    /// Upper bound on the raw value size in bytes. Required, no default.
    pub max_size_bytes: u64,
    /// Advisory rotation cadence in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_period_secs: Option<u64>,
    /// Whether the value must be valid UTF-8.
    #[serde(default)]
    pub utf8_only: bool,
    /// JSON Schema the (UTF-8, JSON) secret value must satisfy, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_schema: Option<serde_json::Value>,
}

impl SecretTypeTraits {
    /// `true` when `mode` is permitted for this type.
    #[must_use]
    pub fn allows_sharing(&self, mode: SharingMode) -> bool {
        self.allow_sharing.contains(&mode)
    }
}

/// Inline `{"type": "array", "items": {"type": "string", "enum": [...]}}`
/// schema for `allow_sharing`. Spelled out (instead of schemars' derived
/// `$ref` to a `oneOf`-of-`const` definition) because the GTS trait
/// validator resolves neither `$defs` references nor `oneOf` branch
/// shapes; `sharing_modes_schema_matches_enum` pins the literals to the
/// serde labels of [`SharingMode`].
fn sharing_modes_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {"type": "string", "enum": ["private", "tenant", "shared"]}
    })
}

impl SecretTypeDescriptor {
    /// `true` when `mode` is permitted for this type.
    #[must_use]
    pub fn allows_sharing(&self, mode: SharingMode) -> bool {
        self.allow_sharing.contains(&mode)
    }

    /// The descriptor's traits in the runtime trait-carrier shape — the same
    /// view the gear resolves from the types-registry for a registered
    /// type. Catalog `value_schema` constants are pinned valid JSON by
    /// `embedded_value_schemas_are_valid_json`; on the impossible parse
    /// failure the trait is omitted.
    #[must_use]
    pub fn traits(&self) -> SecretTypeTraits {
        SecretTypeTraits {
            allow_sharing: self.allow_sharing.to_vec(),
            expirable: self.expirable,
            // usize is at most 64 bits on every supported target: lossless.
            max_size_bytes: self.max_size_bytes as u64,
            rotation_period_secs: self.rotation_period_secs,
            utf8_only: self.utf8_only,
            value_schema: self.value_schema.and_then(|s| serde_json::from_str(s).ok()),
        }
    }
}

const ALL: &[SharingMode] = &[
    SharingMode::Private,
    SharingMode::Tenant,
    SharingMode::Shared,
];
const PRIVATE_ONLY: &[SharingMode] = &[SharingMode::Private];
const PRIVATE_TENANT: &[SharingMode] = &[SharingMode::Private, SharingMode::Tenant];
const TENANT_SHARED: &[SharingMode] = &[SharingMode::Tenant, SharingMode::Shared];
const TENANT_ONLY: &[SharingMode] = &[SharingMode::Tenant];

const OAUTH2_CLIENT_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["client_id", "client_secret"],
  "properties": {
    "client_id": {"type": "string", "minLength": 1},
    "client_secret": {"type": "string", "minLength": 1},
    "token_url": {"type": "string", "minLength": 1},
    "scopes": {"type": "array", "items": {"type": "string"}}
  },
  "additionalProperties": false
}"#;

const BASIC_AUTH_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["username", "password"],
  "properties": {
    "username": {"type": "string", "minLength": 1},
    "password": {"type": "string"}
  },
  "additionalProperties": false
}"#;

/// The full compiled-in type catalog. Order is presentation-only.
pub const SECRET_TYPE_CATALOG: &[SecretTypeDescriptor] = &[
    SecretTypeDescriptor {
        name: "generic",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~"),
        allow_sharing: ALL,
        value_schema: None,
        max_size_bytes: 128 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: false,
    },
    SecretTypeDescriptor {
        name: "api-key",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~"),
        allow_sharing: ALL,
        value_schema: None,
        max_size_bytes: 8 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "personal-token",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.personal_token.v1~"),
        allow_sharing: PRIVATE_ONLY,
        value_schema: None,
        max_size_bytes: 8 * 1024,
        expirable: true,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "oauth2-client",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.oauth2_client.v1~"),
        allow_sharing: TENANT_SHARED,
        value_schema: Some(OAUTH2_CLIENT_SCHEMA),
        max_size_bytes: 16 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "basic-auth",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~"),
        allow_sharing: ALL,
        value_schema: Some(BASIC_AUTH_SCHEMA),
        max_size_bytes: 16 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "bearer-token",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.bearer_token.v1~"),
        allow_sharing: PRIVATE_TENANT,
        value_schema: None,
        max_size_bytes: 64 * 1024,
        expirable: true,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "certificate",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.certificate.v1~"),
        allow_sharing: TENANT_SHARED,
        value_schema: None,
        max_size_bytes: 256 * 1024,
        expirable: true,
        rotation_period_secs: Some(90 * 24 * 3600),
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "ssh-key",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.ssh_key.v1~"),
        allow_sharing: PRIVATE_TENANT,
        value_schema: None,
        max_size_bytes: 64 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "webhook-hmac",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.webhook_hmac.v1~"),
        allow_sharing: TENANT_SHARED,
        value_schema: None,
        max_size_bytes: 8 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
    SecretTypeDescriptor {
        name: "connection-string",
        gts_id: gts_id!("cf.core.credstore.credential.v1~cf.core.credstore.connection_string.v1~"),
        allow_sharing: TENANT_ONLY,
        value_schema: None,
        max_size_bytes: 4 * 1024,
        expirable: false,
        rotation_period_secs: None,
        utf8_only: true,
    },
];

/// A validated secret type: it names a built-in type of the catalog.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SecretType(&'static SecretTypeDescriptor);

impl SecretType {
    /// The built-in `generic` type: opaque secrets.
    #[must_use]
    pub fn generic() -> Self {
        Self(&SECRET_TYPE_CATALOG[0])
    }

    /// Resolve a built-in type by its catalog name (e.g. `"api-key"`); `None` when no
    /// catalog entry matches.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        SECRET_TYPE_CATALOG
            .iter()
            .find(|d| d.name == name)
            .map(Self)
    }

    /// Resolve a type by its full GTS id (the stored representation).
    #[must_use]
    pub fn from_gts_id(id: &str) -> Option<Self> {
        SECRET_TYPE_CATALOG
            .iter()
            .find(|d| d.gts_id == id)
            .map(Self)
    }

    /// Catalog name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0.name
    }

    /// Full derived GTS type id.
    #[must_use]
    pub fn gts_id(self) -> &'static str {
        self.0.gts_id
    }

    /// Deterministic types-registry UUID (v5) of this type — the stored
    /// representation (`credstore_secrets.secret_type_uuid`) and the key for
    /// resolving the type's schema/traits from the registry
    /// ([`type_uuid`] of [`Self::gts_id`]).
    ///
    /// # Panics
    ///
    /// Panics if a catalog `gts_id` is not a valid GTS type id — an impossible
    /// state guarded by `catalog_names_and_ids_are_unique_and_well_formed` and
    /// `type_uuid_is_deterministic_and_matches_registry_v5`.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "catalog gts ids are compile-time constants proven valid by unit tests"
    )]
    pub fn uuid(self) -> Uuid {
        type_uuid(self.0.gts_id).expect("catalog gts id must be a valid GTS type id")
    }

    /// Trait descriptor.
    #[must_use]
    pub fn descriptor(self) -> &'static SecretTypeDescriptor {
        self.0
    }
}

/// Deterministic types-registry UUID (v5) of a GTS type id — matches the id the
/// registry assigns (`GtsId::to_uuid`), so credstore can compute a type's
/// storage/lookup key locally without a registry round-trip. Returns `None`
/// when `gts_id` is not a valid GTS type id.
#[must_use]
pub fn type_uuid(gts_id: &str) -> Option<Uuid> {
    gts::GtsId::try_new(gts_id)
        .ok()
        .map(|parsed| parsed.to_uuid())
}

impl From<SecretType> for gts::GtsId {
    /// A built-in secret type's full GTS type id. Ergonomic for setting
    /// [`crate::CredentialWrite::secret_type`] to a catalog type.
    #[allow(
        clippy::expect_used,
        reason = "catalog gts ids are compile-time constants proven valid by unit tests"
    )]
    fn from(t: SecretType) -> Self {
        gts::GtsId::try_new(t.gts_id()).expect("catalog gts id must be a valid GTS type id")
    }
}

impl fmt::Debug for SecretType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretType").field(&self.0.name).finish()
    }
}

impl fmt::Display for SecretType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.name)
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
