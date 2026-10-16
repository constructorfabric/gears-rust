// Updated: 2026-04-07 by Constructor Tech
// Updated: 2026-03-18 by Constructor Tech
//! Public credential references, values, metadata, sharing, expiry, and write
//! preconditions.
//!
//! Secret values redact formatting output and zeroize their bytes on drop.
use std::fmt;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroize;

use gts::GtsId;

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::field;

/// Re-export from tenant-resolver-sdk for cross-gear type consistency.
pub use tenant_resolver_sdk::TenantId;

/// Owner identifier, representing `SecurityContext.subject_id()`. A row-level
/// access key only — it plays no part in the backend key shape (ADR-0006):
/// the plugin never sees it, and a private row's ownership is enforced by
/// the metadata row alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerId(pub Uuid);

/// Key of one record in the value store (ADR-0006): `(tenant_id, record_id)`.
///
/// `record_id` is the `id` of the record's metadata row, minted at create and
/// never reused. The gear chooses the key; the plugin only maps it to a
/// physical location under its installation prefix. The reference, type and
/// sharing are not part of the key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreKey {
    /// Tenant that owns the record.
    pub tenant_id: TenantId,
    /// The record's identity (the metadata row `id`).
    pub record_id: Uuid,
}

impl StoreKey {
    /// Builds the key of record `record_id` of tenant `tenant_id`.
    #[must_use]
    pub fn new(tenant_id: TenantId, record_id: Uuid) -> Self {
        Self {
            tenant_id,
            record_id,
        }
    }
}

/// Opaque provider-assigned identifier of one stored value version
/// (ADR-0006): a Vault/OpenBao `version`. The provider chooses it, `put`
/// returns it, the gear stores it verbatim in the row's `value_version` and
/// passes it back to `get` and `destroy`. The gear never parses or compares
/// it; ordering is a plugin-internal guarantee (required only together with
/// `destroy`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValueVersion(pub String);

impl ValueVersion {
    /// Wraps a provider version string.
    #[must_use]
    pub fn new(v: impl Into<String>) -> Self {
        Self(v.into())
    }

    /// The provider version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValueVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl OwnerId {
    /// Returns the nil UUID wrapped as an `OwnerId`.
    #[must_use]
    pub fn nil() -> Self {
        Self(Uuid::nil())
    }

    /// Returns `true` if the inner UUID is the nil UUID.
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A validated secret reference key.
///
/// Format: `[a-zA-Z0-9_-]+`, max 255 characters.
/// Kept to one URL-safe token so a reference is usable verbatim as the `{ref}`
/// path segment and in `$filter`; it is never part of the store key (ADR-0006).
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SecretRef(String);

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        SecretRef::new(s).map_err(serde::de::Error::custom)
    }
}

impl SecretRef {
    /// Creates a new `SecretRef` after validating the format.
    ///
    /// # Errors
    ///
    /// Returns a [`CanonicalError::InvalidArgument`] whose single field
    /// violation names `reference` with reason [`field::INVALID_SECRET_REF`]
    /// if the input is empty, exceeds 255 characters, or contains characters
    /// outside `[a-zA-Z0-9_-]`; the same error the gear answers for an invalid
    /// reference.
    #[must_use = "returns a Result that may contain a validation error"]
    pub fn new(value: impl Into<String>) -> Result<Self, CanonicalError> {
        let value = value.into();
        if value.is_empty() {
            return Err(invalid_ref("must not be empty"));
        }
        if value.len() > 255 {
            return Err(invalid_ref("exceeds maximum length of 255 characters"));
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(invalid_ref(
                "contains invalid characters; only [a-zA-Z0-9_-] are allowed",
            ));
        }
        Ok(Self(value))
    }
}

#[resource_error(gts_id!("cf.core.credstore.credential.v1~"))]
struct CredentialScope;

fn invalid_ref(detail: &str) -> CanonicalError {
    CredentialScope::invalid_argument()
        .with_field_violation("reference", detail, field::INVALID_SECRET_REF)
        .create()
}

impl AsRef<str> for SecretRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretRef").field(&self.0).finish()
    }
}

/// A secret value with redacted Debug/Display output.
///
/// Wraps opaque bytes (`Vec<u8>`) and guarantees that content is never
/// leaked through formatting. Does not implement `Serialize`/`Deserialize`
/// to prevent accidental serialization of secret data.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// Creates a new `SecretValue` from raw bytes.
    #[must_use]
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    /// Returns a reference to the raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for SecretValue {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}

impl From<&str> for SecretValue {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Controls the visibility scope of a stored secret.
///
/// Also part of the GTS trait vocabulary: `SecretTypeTraits::allow_sharing`
/// lists the modes a secret type permits, so the derived `x-gts-traits-schema`
/// constrains trait values to this enum (schemars follows the serde
/// `snake_case` renames).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SharingMode {
    /// Only the owner can access the secret.
    Private,
    /// All users within the owner's tenant can access the secret.
    #[default]
    Tenant,
    /// The secret is accessible across tenant boundaries.
    Shared,
}

/// Optimistic-concurrency precondition for `patch`/`delete` — the in-process
/// equivalent of the REST `If-Match` header, and a **required** argument of
/// every update/delete: there are no unconditional overwrites. Read the
/// current generation from a prior [`Credential`]/[`Secret`] validator
/// (`id` + `version`) and send [`Self::Matches`]; a failed precondition
/// surfaces as [`crate::CredStoreError::PreconditionFailed`].
///
/// [`Self::Exists`] is the deliberate, visible-in-code opt-out for blind
/// create-or-replace flows that cannot hold a version (rotation /
/// provisioning, where the new value is not derived from the stored one).
/// Under immutable value versions (ADR-0006) it has no healing role: it
/// carries no meaning beyond RFC 9110 last-writer-wins.
///
/// `put` uses the distinct [`PutPrecondition`] instead, which additionally
/// carries the create-only intent (`If-None-Match: *`, ADR-0007, "PUT: whole replace,
/// secret tri-state").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePrecondition {
    /// The target credential must already exist (REST `If-Match: *`). This is
    /// an explicit last-writer-wins overwrite: concurrent `Exists` writers to
    /// one reference race and the later write survives wholesale — each lands
    /// under its own immutable value version (ADR-0006), so the race
    /// resolves into two intact versions and one pointer, never a corrupted
    /// value. Reserve it for writers that own their references outright
    /// (rotation, provisioning) and for a caller with no version to hold;
    /// read-modify-write callers must use [`Self::Matches`].
    Exists,
    /// Compare-and-set: the current generation must still be `(id, version)`
    /// (REST `If-Match: "<id>.<version>"`). `id` is the row UUID — fresh per
    /// recreated credential — so a validator from an earlier generation never
    /// matches after a delete/recreate even if the version counters coincide.
    Matches {
        /// Row (generation) UUID from the observed [`Validator::id`].
        id: uuid::Uuid,
        /// Version counter from the observed [`Validator::version`].
        version: i64,
    },
}

/// The strong `ETag` source (ADR-0004, D4): a credential row's generation id
/// plus its per-generation monotonic version. Renders on the wire as
/// `"<id>.<version>"`. `Credential::own` is `None` exactly when the
/// caller's tenant holds no row under the reference, in which case the REST
/// layer serves a weak, opaque validator instead (derived from the winning
/// row, never usable in `If-Match`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Validator {
    /// Row (generation) UUID, minted fresh for every recreated credential.
    pub id: Uuid,
    /// Monotonic version counter within this generation.
    pub version: i64,
}

/// Outcome of [`CredStoreClientV2::put`](crate::CredStoreClientV2::put):
/// whether the call created the record (`If-None-Match: *`, 201) or replaced
/// it (204), and the validator to hand back as the response `ETag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PutOutcome {
    /// `true` for a create (`If-None-Match: *`), `false` for a replace.
    pub created: bool,
    /// The written row's fresh validator.
    pub validator: Validator,
}

/// Precondition for [`CredStoreClientV2::put`](crate::CredStoreClientV2::put)
/// (ADR-0007, "Two Write Verbs on One Address"). Unlike [`WritePrecondition`],
/// `put` distinguishes a create-only intent from a guarded or unconditional
/// replace, because `PUT` is the one address that can both create and
/// replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutPrecondition {
    /// `If-None-Match: *` — create-only; `AlreadyExists` if the caller's own
    /// tenant already holds a record under the reference (an inherited
    /// representation does not count, ADR-0007, "PUT: whole replace,
    /// secret tri-state").
    CreateOnly,
    /// `If-Match: *` — replace, last-writer-wins; `PreconditionFailed` if no own record
    /// exists (a `PUT` never creates under this precondition).
    Exists,
    /// `If-Match: "<id>.<version>"` — guarded replace; `PreconditionFailed` if
    /// the validator no longer holds or no own record exists.
    Matches(Validator),
}

/// Suppression policy for the caller's own record (ADR-0008):
/// what a reference means while its record holds no value. Shown only for
/// the caller's own row — an ancestor's policy is not the caller's to see —
/// and is present in [`CredentialWrite`]/[`CredentialPatch`] under the
/// `write` action, the same action that governs `sharing`.
///
/// Wire form: lowercase `"inherit"` / `"none"`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// While the record holds no value, resolution keeps walking up the
    /// tenant chain past it (default).
    #[default]
    Inherit,
    /// While the record holds no value, it blocks resolution outright when
    /// it is the nearest candidate (suppression).
    None,
}

/// The state of the caller's **own row** under a reference (ADR-0004): never a saga state, never `provisioning` or
/// `deprovisioning` — those are invisible to every read.
///
/// Closed on purpose: the REST DTO mirrors it one-to-one, so a new variant must
/// fail to compile there.
///
/// Wire form: lowercase `"declared"` / `"active"` / `"expired"`. The REST
/// `"none"` status (the caller's tenant holds no row under the reference) is
/// produced by the REST layer when [`Credential::own`] is absent; it has no
/// variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    /// The caller's own row exists but carries no value: it was created with
    /// `secret: null` (`PUT` with `SecretWrite::Null`), or the value was
    /// removed (`PATCH {"secret": null}`).
    Declared,
    /// The caller's own row exists and carries a value.
    Active,
    /// The caller's own row is `active` but its `expires_at` has passed.
    /// Derived from `expires_at` at read time, never stored: the record and
    /// its metadata stay visible, its secret is never served (reads of the
    /// secret fail with [`crate::CredStoreError::SecretExpired`]) until the
    /// record is renewed.
    Expired,
}

/// The state of the **effective row** a reference resolves to (ADR-0004),
/// which need not be the caller's own row. Computed at resolution/reduction
/// time, never a stored or filterable column.
///
/// Closed on purpose: the REST DTO mirrors it one-to-one, so a new variant must
/// fail to compile there.
///
/// Wire form: lowercase `"own"` / `"inherited"` / `"overridden"` /
/// `"suppressed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InheritanceStatus {
    /// The winning row is the caller's own, and no ancestor's `shared` row
    /// under the same reference is in play. Also reported when the decisive
    /// row has another type than the record the caller sees (its value is
    /// then not served).
    Own,
    /// The winning row is an ancestor's `shared` row.
    Inherited,
    /// The caller's own row shadows an ancestor's `shared` row under the same
    /// reference.
    Overridden,
    /// The winning row is a value-less record with `fallback: none` — the
    /// walk stops there rather than falling through (own or an ancestor's).
    Suppressed,
}

/// The caller's **own row** under a reference: its state and write metadata.
/// Populated by the own row alone, even when the effective value is inherited
/// (`inheritance: inherited` with a `declared` own row), never by an
/// ancestor's row: an inherited entry must not disclose identifiers from
/// another tenant, and a caller who needs the ancestor's owner acts in that
/// tenant's context and reads it there.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnRow {
    /// State of the own row: `declared`/`active`/`expired`.
    pub status: CredentialStatus,
    /// Suppression policy of the own row.
    pub fallback: Fallback,
    /// Last-write instant of the own row.
    pub updated_at: OffsetDateTime,
    /// Subject id that created the own row.
    pub owner_id: OwnerId,
    /// The own row's validator (the strong `ETag` source); its `version` is
    /// the row's monotonic version.
    pub validator: Validator,
}

/// The addressable **credential record** (ADR-0004):
/// reference, type, sharing, the effective-row state, and the caller's own
/// row when there is one — never the value, and never the owning tenant. `GET /credentials/{ref}`'s response
/// shape and the collection item's shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Credential {
    /// The caller-chosen reference this record answers to.
    pub reference: SecretRef,
    /// The resolved record's full GTS type id (the effective record's type —
    /// the caller's own row's type if it has one, else the winner's). A value
    /// is only ever served under a record of its own type: a decisive row of
    /// another type yields no value and inheritance `own`.
    pub secret_type: String,
    /// Sharing mode of the effective record.
    pub sharing: SharingMode,
    /// State of the effective row this reference resolves to.
    pub inheritance: InheritanceStatus,
    /// Expiry instant of the effective record, when the type is expirable
    /// and one was set.
    pub expires_at: Option<OffsetDateTime>,
    /// The caller's own row; `None` when the caller's tenant holds no row
    /// under the reference (the REST layer then serves status `none` and a
    /// weak, opaque `ETag`).
    pub own: Option<OwnRow>,
}

/// The value, with exactly what is needed to use it (ADR-0004): nothing administrative rides along — `sharing`,
/// `inheritance`, `status` stay on [`Credential`]. The SDK-side convenience
/// envelope [`CredStoreClientV2::get_secret`](crate::CredStoreClientV2::get_secret)
/// wraps, projecting `GET /credentials/{ref}?$select=reference,type,expires_at,secret`
/// to these four wire fields plus the row's validator (ADR-0004, "Read actions
/// follow the projection") — the shape the
/// withdrawn `GET /credentials/{ref}/secret` used to return.
#[derive(Debug)]
#[allow(
    clippy::struct_field_names,
    reason = "secret_type names the field precisely; Secret is the resource this schema is for"
)]
pub struct Secret {
    /// The reference this value was resolved through.
    pub reference: SecretRef,
    /// The resolved value's full GTS type id (a consumer must know whether
    /// it is parsing a `basic_auth` object or an `api_key` string). Always the
    /// type of the record the value is served under; a decisive row of another
    /// type yields no value.
    pub secret_type: String,
    /// Expiry instant, when the type is expirable and one was set.
    pub expires_at: Option<OffsetDateTime>,
    /// The decrypted value.
    pub secret: SecretValue,
    /// The winning row's validator — travels with the value so a caller that
    /// reads and rotates its own credential never needs the record address.
    pub validator: Validator,
}

/// The secret part of a [`CredentialWrite`], tri-state once past the REST
/// gate (a wire body that omits `secret` is rejected before this type is
/// built).
///
/// `Set` stores a new value version. `Null` on create makes a value-less
/// (`declared`) record; on a replace it removes the stored value (needs
/// `write_secret` when a value existed) — a removal is always an explicit
/// decision. To change metadata (for example `sharing`) without touching the
/// secret, use `patch`.
#[derive(Debug)]
pub enum SecretWrite {
    /// Store this value as a new version.
    Set(SecretValue),
    /// Write no value: a value-less create, or a removal on replace.
    Null,
}

/// Body of [`CredStoreClientV2::put`](crate::CredStoreClientV2::put) — a
/// whole-credential replace: fields absent reset to their defaults (ADR-0007,
/// "PUT: whole replace, secret tri-state"). `secret` is required
/// at the REST boundary (its *absence* on the wire is 400 `SECRET_REQUIRED`,
/// before this type is even built); past that gate it is a [`SecretWrite`]:
/// `Set` writes a value (create, or replace/rotate), `Null` writes none: on
/// create the row is inserted `declared`; on replace of an `active` row the
/// value is removed in the same one transaction `PATCH {"secret": null}`
/// uses; on replace of an already-`declared` row nothing about the value
/// changes.
#[derive(Debug)]
pub struct CredentialWrite {
    /// Full GTS type id. Required on create: nothing is defaulted, and a
    /// create without a type is rejected with `TYPE_REQUIRED` (REST: 400);
    /// on replace it must equal the stored type (`TYPE_IMMUTABLE` otherwise).
    pub secret_type: Option<GtsId>,
    /// Sharing mode for the written record.
    pub sharing: SharingMode,
    /// Suppression policy; defaults to [`Fallback::Inherit`].
    pub fallback: Fallback,
    /// Expiry instant. `put` replaces the whole record, so `None` clears any
    /// stored expiry. To rotate the secret and keep the expiry, `patch` with
    /// only `secret`.
    pub expires_at: Option<OffsetDateTime>,
    /// The secret to write: a new value, or an explicit `Null`. The REST
    /// layer maps the wire's *absent* `secret` key to 400 (`SECRET_REQUIRED`)
    /// before this type is ever constructed, so `Null` here is unambiguously
    /// an explicit `null`, never "the caller forgot the field".
    pub secret: SecretWrite,
}

/// Tri-state field for an RFC 7396 JSON Merge Patch: absent (untouched),
/// explicit `null` (remove/clear), or a value (replace). Pair with
/// `#[serde(default, deserialize_with = "…")]` at the REST DTO boundary — see
/// `credstore::api::rest::dto::deserialize_double_option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchField<T> {
    /// The key was not present in the merge-patch body; leave untouched.
    #[default]
    Absent,
    /// The key was present with JSON `null`; remove/clear the field.
    Null,
    /// The key was present with a value; replace the field with it.
    Set(T),
}

impl<T> PatchField<T> {
    /// `true` iff the key was not present in the body at all.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Body of [`CredStoreClientV2::patch`](crate::CredStoreClientV2::patch) — an
/// RFC 7396 JSON Merge Patch over the mutable fields of [`Credential`] plus
/// `secret` (ADR-0007, "PATCH: merge, never creates"). A field absent from every one of these is untouched; a
/// `PatchField::Null` on `expires_at`/`secret` clears/removes it. `sharing`,
/// `fallback` and `secret_type` have no `Null` state on the wire — a
/// merge-patch `null` for any of them is a REST-layer 400
/// (`NULL_NOT_ALLOWED`), since none is a nullable column.
#[derive(Debug, Default)]
pub struct CredentialPatch {
    /// Present only to be compared against the stored type; a differing
    /// value is `TYPE_IMMUTABLE`. Never actually changes the stored type.
    pub secret_type: Option<GtsId>,
    /// New sharing mode, if the merge-patch body named one.
    pub sharing: Option<SharingMode>,
    /// New suppression policy, if the merge-patch body named one.
    pub fallback: Option<Fallback>,
    /// Expiry change: absent (untouched), `Null` (clear), or `Set` (replace).
    pub expires_at: PatchField<OffsetDateTime>,
    /// Value change: absent (untouched), `Null` (remove — the record becomes
    /// `declared`), or `Set` (rotate/create the value).
    pub secret: PatchField<SecretValue>,
}

impl CredentialPatch {
    /// `true` when the patch touches nothing at all — every field is
    /// [`PatchField::Absent`] and no metadata key was named. Rejected by the
    /// domain with `EMPTY_PATCH` (400): `PATCH {}` is meaningless, not a
    /// no-op success.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secret_type.is_none()
            && self.sharing.is_none()
            && self.fallback.is_none()
            && self.expires_at.is_absent()
            && self.secret.is_absent()
    }
}

/// One item of the collection read (`CredStoreClientV2::list`, ADR-0005): the
/// reduced [`Credential`] a reference resolves to, plus its decrypted value
/// (`secret`) when the request selected it (`$select` containing `secret`,
/// ADR-0004, "Read actions follow the projection"). `secret` is `None` otherwise — the collection never carries a
/// value unless the caller opted in, and even then only for the items whose
/// value the caller may read.
#[derive(Debug)]
pub struct CredentialListItem {
    /// The reduced credential record (ADR-0005, "Reducing a reference to one
    /// item"): one item per reference, matching what a point read
    /// (`CredStoreClientV2::get_record`) of that reference would resolve to.
    pub credential: Credential,
    /// The decrypted value, present only when `secret` is selected and only for an item
    /// the caller may read (`read_secret`); an item the caller may not read
    /// is omitted from the page entirely, and an expired item is returned
    /// with `None` here.
    pub secret: Option<SecretValue>,
}

#[cfg(test)]
mod models_tests {
    use super::*;

    #[test]
    fn secret_ref_accepts_valid_shapes() {
        for ok in ["partner-openai-key", "api_key_v2", "ABC123", "ok-key_1"] {
            assert!(SecretRef::new(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn secret_ref_rejects_invalid_chars_and_empty() {
        assert!(SecretRef::new("").is_err());
        for bad in ["has:colon", "my key", "key/path"] {
            assert!(SecretRef::new(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn secret_ref_length_boundary() {
        // 255 is the inclusive max; 256 is rejected (boundary both sides).
        assert!(SecretRef::new("a".repeat(255)).is_ok());
        assert!(SecretRef::new("a".repeat(256)).is_err());
    }

    #[test]
    fn secret_ref_deserialize_validates() {
        // The custom Deserialize is the real wire path (a raw JSON string must
        // go through the same validation as `SecretRef::new`).
        let valid: Result<SecretRef, _> = serde_json::from_str("\"valid-key_1\"");
        assert_eq!(valid.expect("valid").as_ref(), "valid-key_1");
        assert!(serde_json::from_str::<SecretRef>("\"my:evil/key\"").is_err());
        assert!(serde_json::from_str::<SecretRef>("\"\"").is_err());
        assert!(serde_json::from_str::<SecretRef>(&format!("\"{}\"", "a".repeat(256))).is_err());
    }

    #[test]
    fn secret_ref_serde_round_trips() {
        let r = SecretRef::new("round-trip").expect("valid");
        let json = serde_json::to_string(&r).expect("serialize");
        assert_eq!(json, "\"round-trip\"");
        let back: SecretRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.as_ref(), "round-trip");
    }

    #[test]
    fn secret_value_redacts() {
        let v = SecretValue::from("supersecret");
        assert_eq!(format!("{v:?}"), "[REDACTED]");
        assert_eq!(format!("{v}"), "[REDACTED]");
        assert_eq!(v.as_bytes(), b"supersecret");
    }

    #[test]
    fn value_version_is_opaque_and_displays_verbatim() {
        let v = ValueVersion::new("7");
        assert_eq!(v.as_str(), "7");
        assert_eq!(v.to_string(), "7");
        assert_eq!(v, ValueVersion("7".to_owned()));
    }

    #[test]
    fn store_key_is_a_hashable_value() {
        let k = StoreKey::new(TenantId::nil(), Uuid::nil());
        assert_eq!(k, k.clone());
        let mut set = std::collections::HashSet::new();
        set.insert(k.clone());
        assert!(set.contains(&k));
    }

    #[test]
    fn sharing_mode_default_is_tenant() {
        assert_eq!(SharingMode::default(), SharingMode::Tenant);
    }

    #[test]
    fn sharing_mode_serde_round_trips() {
        for (mode, expected) in [
            (SharingMode::Private, "\"private\""),
            (SharingMode::Tenant, "\"tenant\""),
            (SharingMode::Shared, "\"shared\""),
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            assert_eq!(json, expected);
            let back: SharingMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn fallback_default_is_inherit_and_wire_form_is_lowercase() {
        assert_eq!(Fallback::default(), Fallback::Inherit);
        for (f, expected) in [
            (Fallback::Inherit, "\"inherit\""),
            (Fallback::None, "\"none\""),
        ] {
            let json = serde_json::to_string(&f).expect("serialize");
            assert_eq!(json, expected);
            let back: Fallback = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, f);
        }
    }

    #[test]
    fn credential_status_wire_form_is_lowercase() {
        for (s, expected) in [
            (CredentialStatus::Declared, "\"declared\""),
            (CredentialStatus::Active, "\"active\""),
            (CredentialStatus::Expired, "\"expired\""),
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn inheritance_status_wire_form_is_lowercase() {
        for (s, expected) in [
            (InheritanceStatus::Own, "\"own\""),
            (InheritanceStatus::Inherited, "\"inherited\""),
            (InheritanceStatus::Overridden, "\"overridden\""),
            (InheritanceStatus::Suppressed, "\"suppressed\""),
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn patch_field_default_is_absent() {
        assert!(PatchField::<i64>::default().is_absent());
        assert!(!PatchField::<i64>::Null.is_absent());
        assert!(!PatchField::Set(3_i64).is_absent());
    }

    #[test]
    fn credential_patch_is_empty_iff_every_field_is_untouched() {
        assert!(CredentialPatch::default().is_empty());

        assert!(
            !CredentialPatch {
                sharing: Some(SharingMode::Shared),
                ..CredentialPatch::default()
            }
            .is_empty()
        );

        assert!(
            !CredentialPatch {
                expires_at: PatchField::Null,
                ..CredentialPatch::default()
            }
            .is_empty()
        );

        assert!(
            !CredentialPatch {
                secret: PatchField::Set(SecretValue::from("x")),
                ..CredentialPatch::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn credential_list_item_carries_no_secret_in_metadata_mode() {
        let credential = Credential {
            reference: SecretRef::new("ref").expect("valid"),
            secret_type: crate::types::SecretType::generic().gts_id().to_owned(),
            sharing: SharingMode::Tenant,
            inheritance: InheritanceStatus::Own,
            expires_at: None,
            own: Some(OwnRow {
                status: CredentialStatus::Active,
                fallback: Fallback::Inherit,
                updated_at: OffsetDateTime::UNIX_EPOCH,
                owner_id: OwnerId::nil(),
                validator: Validator {
                    id: Uuid::nil(),
                    version: 1,
                },
            }),
        };
        let item = CredentialListItem {
            credential,
            secret: None,
        };
        assert!(item.secret.is_none());
    }

    #[test]
    fn secret_write_debug_redacts_the_value() {
        let set = SecretWrite::Set(SecretValue::from("s3cr3t"));
        assert!(!format!("{set:?}").contains("s3cr3t"));
        assert_eq!(format!("{:?}", SecretWrite::Null), "Null");
    }

    #[test]
    fn owner_id_nil_is_the_sentinel_for_no_owner() {
        let nil = OwnerId::nil();
        assert!(nil.is_nil());
        assert_eq!(nil.to_string(), Uuid::nil().to_string());
        assert!(!OwnerId(Uuid::new_v4()).is_nil());
    }

    #[test]
    fn secret_ref_debug_shows_the_reference_it_wraps() {
        let key = SecretRef::new("openai-key").expect("valid");
        assert_eq!(format!("{key:?}"), "SecretRef(\"openai-key\")");
    }

    #[test]
    fn secret_value_carries_raw_bytes_from_either_constructor() {
        let from_new = SecretValue::new(b"s3cr3t".to_vec());
        assert_eq!(from_new.as_bytes(), b"s3cr3t");
        let from_vec: SecretValue = b"s3cr3t".to_vec().into();
        assert_eq!(from_vec.as_bytes(), from_new.as_bytes());
    }
}
