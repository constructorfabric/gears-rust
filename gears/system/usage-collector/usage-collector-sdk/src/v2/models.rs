//! Foundation domain models for the Usage Collector SDK.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use gts::GtsTypeId;

use crate::v2::error::UsageCollectorError;
use crate::v2::quantity::UsageQuantity;

// MetadataKey

/// Character cap on a metadata key, from the `RecordMetadata` schema's
/// `propertyNames` in `docs/usage-collector-v2.yaml`.
const MAX_METADATA_KEY_LEN: usize = 128;

/// Validating newtype over a metadata key string.
///
/// Keys are domain-opaque, so the SDK encodes no casing or charset policy.
/// Closed-shape membership against the resolved meter declaration's
/// `metadata_fields` is a gateway-time check.
///
/// # Validation
///
/// - Non-empty.
/// - At most 128 characters (`MAX_METADATA_KEY_LEN`).
/// - No ASCII control characters, DEL included — which covers the NUL the
///   Postgres `jsonb` key requirement excludes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MetadataKey(String);

impl MetadataKey {
    /// Creates a [`MetadataKey`] after validating the value is non-empty, at
    /// most 128 characters, and free of ASCII control characters. The bound
    /// is load-bearing: a key reaches storage as a `jsonb` object key and
    /// comes back as an `r.metadata ->> $N` bind on both read paths.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when the input is
    /// empty, longer than 128 characters, or carries an ASCII control
    /// character (including NUL and DEL).
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        if raw.is_empty() {
            return Err(UsageCollectorError::invalid_metadata_key(
                "metadata key must not be empty",
            ));
        }
        if raw.chars().count() > MAX_METADATA_KEY_LEN {
            return Err(UsageCollectorError::invalid_metadata_key(
                "metadata key must be at most 128 characters",
            ));
        }
        // `char::is_ascii_control()` covers U+007F (DEL) and the NUL a
        // `jsonb` key cannot carry, so neither needs its own check.
        if raw.chars().any(|c| c.is_ascii_control()) {
            return Err(UsageCollectorError::invalid_metadata_key(
                "metadata key must not contain ASCII control characters",
            ));
        }
        Ok(Self(raw))
    }

    /// Borrows the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype and returns the owned string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for MetadataKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MetadataKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MetadataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MetadataKey {
    type Err = UsageCollectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for MetadataKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        MetadataKey::new(raw).map_err(serde::de::Error::custom)
    }
}

// Attribution composites

/// Character cap on each attribution component, from the `ResourceRefV2` and
/// `SubjectRefV2` schemas (`maxLength: 256`) in `docs/usage-collector-v2.yaml`.
const MAX_ATTRIBUTION_LEN: usize = 256;

/// Validates one attribution component: non-empty, no ASCII control
/// character, at most [`MAX_ATTRIBUTION_LEN`] characters.
///
/// `field` is the component's dotted path and every rule here reports it, so
/// one component yields one `field_violations[].field` spelling whichever rule
/// fired — a client maps a violation back to its own form without knowing
/// which did.
///
/// The control-character rule is load-bearing on the read path, not wire
/// tidiness: `resource_id` and `resource_type` are admissible `$orderby` keys,
/// and [`crate::v2::Keyset::new`] measures the *JSON-encoded* boundary values
/// against [`crate::v2::MAX_KEYSET_BYTES`] — a budget derived against 4-byte
/// scalars at the character cap. A control character costs six bytes there
/// against such a scalar's four, so an ingested one makes a later order on its
/// field a `KeysetInvalid::TooLarge`, which a plugin maps to `Internal`: a 500
/// the caller provoked on a read. Refused here, it is a 400 at ingestion
/// instead. Pinned by
/// `models_tests::no_admissible_attribution_value_can_outgrow_the_keyset_budget`.
fn check_attribution_component(field: &str, value: &str) -> Result<(), UsageCollectorError> {
    if value.is_empty() {
        return Err(UsageCollectorError::invalid_attribution_component(
            field,
            format!("{field} must not be empty"),
        ));
    }
    if value.chars().any(|c| c.is_ascii_control()) {
        return Err(UsageCollectorError::invalid_attribution_component(
            field,
            format!("{field} must not contain ASCII control characters"),
        ));
    }
    if value.chars().count() > MAX_ATTRIBUTION_LEN {
        return Err(UsageCollectorError::attribution_too_long(
            field,
            MAX_ATTRIBUTION_LEN,
        ));
    }
    Ok(())
}

/// Reference to the resource instance to which usage is attributed.
/// Mandatory on every usage record.
///
/// Both `resource_id` and `resource_type` are validated non-empty, at most
/// 256 characters, and free of ASCII control characters — the NUL a Postgres
/// `text` column cannot carry among them, though
/// [`check_attribution_component`] has the load-bearing reason.
/// `Deserialize` routes through [`Self::new`] so wire payloads cannot bypass
/// the invariant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ResourceRef {
    /// Resource instance identifier inside the attributed tenant scope.
    resource_id: String,
    /// Type discriminator such as `compute.vm`.
    resource_type: String,
}

impl ResourceRef {
    /// Creates a [`ResourceRef`] after validating both components are
    /// non-empty, carry no ASCII control character, and are not longer than
    /// 256 characters.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when `resource_id`
    /// or `resource_type` is empty, carries an ASCII control character
    /// (NUL and DEL included), or is longer than 256 characters.
    pub fn new(
        resource_id: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Result<Self, UsageCollectorError> {
        let resource_id = resource_id.into();
        check_attribution_component("resource_ref.resource_id", &resource_id)?;
        let resource_type = resource_type.into();
        check_attribution_component("resource_ref.resource_type", &resource_type)?;
        Ok(Self {
            resource_id,
            resource_type,
        })
    }

    /// Borrows the resource instance identifier.
    #[must_use]
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    /// Borrows the resource-type discriminator.
    #[must_use]
    pub fn resource_type(&self) -> &str {
        &self.resource_type
    }
}

impl<'de> Deserialize<'de> for ResourceRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            resource_id: String,
            resource_type: String,
        }
        let Raw {
            resource_id,
            resource_type,
        } = Raw::deserialize(deserializer)?;
        ResourceRef::new(resource_id, resource_type).map_err(serde::de::Error::custom)
    }
}

/// Optional reference to the principal to which usage is attributed.
/// Caller-supplied and never derived from the caller `SecurityContext`.
///
/// `subject_id` is validated non-empty and control-character-free, and
/// `subject_type` the same way when supplied (an explicit `Some("")` is
/// rejected). Neither component is a keyset key, so the exclusion is
/// [`ResourceRef`]'s applied to attribution reaching the same columns.
/// `Deserialize` routes through [`Self::new`] so wire payloads cannot bypass
/// either invariant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SubjectRef {
    /// Internal platform identifier issued by the identity layer.
    subject_id: String,
    /// Optional type discriminator for systems with subject-type taxonomies.
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_type: Option<String>,
}

impl SubjectRef {
    /// Creates a [`SubjectRef`] after validating `subject_id` is non-empty
    /// and `subject_type`, when supplied, is non-empty; both components are
    /// also validated to carry no ASCII control character and to be no
    /// longer than 256 characters.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when `subject_id`
    /// is empty, `subject_type` is `Some("")`, either component carries an
    /// ASCII control character (NUL and DEL included), or either is longer
    /// than 256 characters.
    pub fn new(
        subject_id: impl Into<String>,
        subject_type: Option<impl Into<String>>,
    ) -> Result<Self, UsageCollectorError> {
        let subject_id = subject_id.into();
        check_attribution_component("subject_ref.subject_id", &subject_id)?;
        let subject_type = match subject_type {
            None => None,
            Some(s) => {
                let s = s.into();
                check_attribution_component("subject_ref.subject_type", &s)?;
                Some(s)
            }
        };
        Ok(Self {
            subject_id,
            subject_type,
        })
    }

    /// Borrows the subject identifier.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    /// Borrows the optional subject-type discriminator.
    #[must_use]
    pub fn subject_type(&self) -> Option<&str> {
        self.subject_type.as_deref()
    }
}

impl<'de> Deserialize<'de> for SubjectRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            subject_id: String,
            #[serde(default)]
            subject_type: Option<String>,
        }
        let Raw {
            subject_id,
            subject_type,
        } = Raw::deserialize(deserializer)?;
        SubjectRef::new(subject_id, subject_type).map_err(serde::de::Error::custom)
    }
}

// Idempotency key

/// Ceiling on the wire length of an idempotency key, from the
/// `IdempotencyKey` schema in `docs/usage-collector-v2.yaml`.
const MAX_IDEMPOTENCY_KEY_LEN: usize = 256;

/// Validating newtype over the caller-supplied idempotency key string.
///
/// A dedup-identity input ([`crate::v2::id::derive_usage_record_id`]), so a
/// malformed key must not reach the derivation.
///
/// No prefix is reserved: both entry kinds carry a caller-supplied key and an
/// invalidation repeats its target's, so `entry_type` — not the key — is what
/// tells the two apart.
///
/// # Validation
///
/// - Non-empty.
/// - At most 256 characters (`MAX_IDEMPOTENCY_KEY_LEN`).
/// - No ASCII control characters, DEL included — the wire contract's
///   `^[^\x00-\x1F\x7F]+$`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Creates an [`IdempotencyKey`] after validating it against the wire
    /// contract's `IdempotencyKey` schema (`minLength: 1`,
    /// `maxLength: 256`, `pattern: ^[^\x00-\x1F\x7F]+$`).
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when the input is
    /// empty, longer than 256 characters, or carries an ASCII control
    /// character (including DEL).
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        validate_idempotency_key(&raw)?;
        Ok(Self(raw))
    }

    /// Rebuilds a key read back from storage.
    ///
    /// Delegates to [`Self::new`]; the separate spelling names provenance
    /// only.
    ///
    /// # Errors
    ///
    /// [`UsageCollectorError::InvalidArgument`] when the stored value is
    /// empty, longer than 256 characters, or carries an ASCII control
    /// character.
    pub fn from_stored(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        Self::new(value)
    }

    /// Borrows the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype and returns the owned string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// Every check an [`IdempotencyKey`] meets, whichever constructor built it:
/// non-empty, at most [`MAX_IDEMPOTENCY_KEY_LEN`] characters, and no ASCII
/// control character (DEL included).
fn validate_idempotency_key(raw: &str) -> Result<(), UsageCollectorError> {
    if raw.is_empty() {
        return Err(UsageCollectorError::invalid_idempotency_key(
            "idempotency_key must not be empty",
        ));
    }
    if raw.chars().count() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(UsageCollectorError::invalid_idempotency_key(
            "idempotency_key must be at most 256 characters",
        ));
    }
    // `char::is_ascii_control()` covers U+007F (DEL) alongside U+0000..=U+001F.
    // Load-bearing, not cosmetic: [`crate::v2::id::derive_usage_record_id`] joins
    // this value with the other identity inputs under a `0x1F` separator, and
    // the key is not the final field, so a control character inside it would
    // inject a separator into the middle of the pre-image.
    if raw.chars().any(|c| c.is_ascii_control()) {
        return Err(UsageCollectorError::invalid_idempotency_key(
            "idempotency_key must not contain ASCII control characters",
        ));
    }
    Ok(())
}

impl AsRef<str> for IdempotencyKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IdempotencyKey {
    type Err = UsageCollectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        IdempotencyKey::new(raw).map_err(serde::de::Error::custom)
    }
}

// ReasonCode

/// Character cap on an invalidation reason code, from the wire contract's
/// `ReasonCode` schema (`maxLength: 128`).
const MAX_REASON_CODE_LEN: usize = 128;

/// Validating newtype over the caller-supplied invalidation reason code.
///
/// The vocabulary is deliberately open — the gear records the emitter's
/// stated intent and infers nothing from it — so no closed enum is declared
/// here or on the wire.
///
/// # Validation
///
/// - Non-empty, and not whitespace only — an absent, empty or
///   whitespace-only code is rejected
///   (`cpt-cf-usage-collector-dod-mandatory-reason-code`,
///   `cpt-cf-usage-collector-algo-entry-type-discrimination`). Only a code
///   with no content at all; the value is stored untrimmed.
/// - At most 128 characters (`MAX_REASON_CODE_LEN`).
/// - No ASCII control characters, DEL included. Wire hygiene rather than
///   pre-image safety: the code is not an identity input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReasonCode(String);

impl ReasonCode {
    /// Creates a [`ReasonCode`] after validating it against the wire
    /// contract's `ReasonCode` schema (`minLength: 1`, `maxLength: 128`,
    /// `pattern` refusing an all-whitespace code).
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when the input is
    /// empty or whitespace only, longer than 128 characters, or carries an
    /// ASCII control character (including DEL).
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        if raw.is_empty() {
            return Err(UsageCollectorError::invalid_reason_code(
                "reason_code must not be empty",
            ));
        }
        // Checked, not applied: nothing downstream trims it either.
        if raw.trim().is_empty() {
            return Err(UsageCollectorError::invalid_reason_code(
                "reason_code must not be whitespace only",
            ));
        }
        if raw.chars().count() > MAX_REASON_CODE_LEN {
            return Err(UsageCollectorError::invalid_reason_code(
                "reason_code must be at most 128 characters",
            ));
        }
        // `char::is_ascii_control()` covers U+007F (DEL) alongside
        // U+0000..=U+001F.
        if raw.chars().any(|c| c.is_ascii_control()) {
            return Err(UsageCollectorError::invalid_reason_code(
                "reason_code must not contain ASCII control characters",
            ));
        }
        Ok(Self(raw))
    }

    /// Borrows the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype and returns the owned string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ReasonCode {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ReasonCode {
    type Err = UsageCollectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for ReasonCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ReasonCode::new(raw).map_err(serde::de::Error::custom)
    }
}

// MeterTypeId

/// The GTS base type every meter derives from.
///
/// Alias of [`crate::v2::gts::USAGE_RECORD_RESOURCE`] — the same string, not a
/// new identifier — so a meter-type call site can name the base in
/// meter-type terms.
pub const USAGE_RECORD_BASE_TYPE: &str = crate::v2::gts::USAGE_RECORD_RESOURCE;

/// Ceiling on the wire length of a meter type identifier, from
/// `docs/schemas/usage_record.v2.schema.json`'s `gts_type_id` property.
const MAX_METER_TYPE_ID_LEN: usize = 512;

/// Reference to the GTS type declaration a ledger entry is metered against.
///
/// A meter is a derived **type** of [`USAGE_RECORD_BASE_TYPE`] with exactly
/// one further segment — not an instance — so this wraps a [`GtsTypeId`].
/// The declaration it names is owned by `types-registry`; this gear resolves
/// it and mints none.
///
/// The gear infers no metering meaning from the shape of the identifier.
/// Fold, canonical unit, and metadata surface come from the resolved
/// declaration alone.
///
/// Deliberately implements neither `Ord` nor `PartialOrd`: the wrapped
/// [`GtsTypeId`] implements neither. A caller that needs this as a
/// `BTreeMap`/`BTreeSet` key must add manual impls delegating to the string
/// form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MeterTypeId(GtsTypeId);

impl MeterTypeId {
    /// Creates a [`MeterTypeId`] after validating it against the base type's
    /// published pattern
    /// (`^gts\.cf\.core\.uc\.usage_record\.v2~[^\x00-\x1F\x7F~]+~$`,
    /// `maxLength: 512`).
    ///
    /// Length and control characters are checked before the value is handed
    /// to [`GtsTypeId::try_new`], so the diagnostic names the real problem
    /// rather than a generic GTS parse error. The control-character exclusion
    /// is load-bearing: [`crate::v2::id::derive_usage_record_id`] joins this value
    /// with the other identity inputs under a `0x1F` separator, so a control
    /// character inside it would let two distinct identities collapse to one
    /// pre-image.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when the value is
    /// longer than 512 bytes, carries an ASCII control character (including
    /// DEL), does not derive from [`USAGE_RECORD_BASE_TYPE`], is not
    /// `~`-terminated, does not add exactly one further derivation segment
    /// (empty, or containing an interior `~`) to the base, or — having
    /// passed all of the above — fails the per-segment GTS grammar enforced
    /// by the delegated [`GtsTypeId::try_new`] call.
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();

        if raw.len() > MAX_METER_TYPE_ID_LEN {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must be at most 512 bytes",
            ));
        }

        // `char::is_ascii_control()` already covers U+007F (DEL) alongside
        // U+0000..=U+001F, so no separate DEL check is needed.
        if raw.chars().any(|c| c.is_ascii_control()) {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must not contain ASCII control characters",
            ));
        }

        let Some(suffix) = raw.strip_prefix(USAGE_RECORD_BASE_TYPE) else {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must derive from `gts.cf.core.uc.usage_record.v2~`",
            ));
        };

        // Exactly one further segment: non-empty, `~`-terminated, and with
        // no interior `~` that would make it two.
        if suffix.is_empty() {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must add exactly one derivation segment to the base type",
            ));
        }
        let Some(segment) = suffix.strip_suffix('~') else {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must end with `~`",
            ));
        };
        if segment.is_empty() || segment.contains('~') {
            return Err(UsageCollectorError::invalid_meter_type_id(
                &raw,
                "must add exactly one derivation segment to the base type",
            ));
        }

        let parsed = GtsTypeId::try_new(&raw)
            .map_err(|e| UsageCollectorError::invalid_meter_type_id(&raw, &e.to_string()))?;

        Ok(Self(parsed))
    }

    /// Borrows the wire string, terminator included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// Borrows the underlying GTS type identifier.
    #[must_use]
    pub fn as_gts(&self) -> &GtsTypeId {
        &self.0
    }
}

impl AsRef<str> for MeterTypeId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for MeterTypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MeterTypeId {
    type Err = UsageCollectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for MeterTypeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        MeterTypeId::new(raw).map_err(serde::de::Error::custom)
    }
}

// Usage-record exchange types

/// Wire name of the covered period's inclusive start bound.
pub const WINDOW_START_FIELD: &str = "window_start";

/// Wire name of the covered period's exclusive end bound.
pub const WINDOW_END_FIELD: &str = "window_end";

/// Wire name of the record identifier, and the second half of the gateway's
/// canonical keyset `(window_end, id)`.
pub const RECORD_ID_FIELD: &str = "id";

/// The REST path of the dedicated backfill route, named by the live path's
/// past-tolerance rejection. Must stay in step with
/// `usage-collector-v2.yaml`, which carries the path independently.
pub const BACKFILL_ROUTE_PATH: &str = "/usage-collector/v2/records/backfill";

/// The closed discriminator between a measurement and a withdrawal.
///
/// **Caller-supplied on the ingestion shape**, required and never inferred:
/// an invalidation repeats its target's idempotency key, so a withdrawal
/// stripped of its discriminator would be absorbed as a retry of the entry it
/// means to withdraw. This field and `reason_code` must agree, and both
/// projections refuse a submission where they do not.
///
/// **Derived on the persisted shape.** [`UsageRecord::entry_type`] reads it
/// off the [`Invalidation`] the entry carries, so an accepted entry has one
/// place its kind can be read
/// (`cpt-cf-usage-collector-adr-append-only-invalidation`).
///
/// The kind is never read off the quantity: a zero or negative quantity is an
/// ordinary measurement, and an invalidation echoes the quantity it withdraws
/// rather than negating it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    /// An ordinary measurement: the entry carries no `invalidates`.
    Record,
    /// A withdrawal: the entry names the record it invalidates.
    Invalidation,
}

impl EntryType {
    /// Every variant, so a guard over the closed label set cannot fall
    /// behind the enum.
    pub const ALL: &'static [Self] = &[Self::Record, Self::Invalidation];

    /// The wire spelling, shared by the REST projection and the `$filter`
    /// surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Invalidation => "invalidation",
        }
    }

    /// Every admissible wire label, derived from [`Self::ALL`] via
    /// [`Self::as_str`] rather than hand-listed a second time.
    #[must_use]
    pub const fn wire_labels() -> &'static [&'static str] {
        const LABELS: [&str; EntryType::ALL.len()] = {
            let mut labels = [""; EntryType::ALL.len()];
            let mut i = 0;
            while i < EntryType::ALL.len() {
                labels[i] = EntryType::ALL[i].as_str();
                i += 1;
            }
            labels
        };
        &LABELS
    }
}

/// Which ingestion path admitted an entry.
///
/// **Server-assigned, never caller-supplied.** The Ingestion Gateway stamps
/// it from the route the entry arrived on
/// (`cpt-cf-usage-collector-adr-backfill-isolation`), so it is absent from
/// [`CreateUsageRecord`] and refused by that type's `deny_unknown_fields`
/// wire shadow.
///
/// It applies to invalidations exactly as to measurements: the covered-period
/// bounds belong to the path rather than the entry kind, so a withdrawal of a
/// period older than the live past tolerance reads `backfill`.
///
/// Closed, and deliberately so: it is also a bounded metric label on
/// `uc_ingestion_records_total` and `uc_ingestion_duration_seconds`, so an
/// open vocabulary here would be unbounded cardinality there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordOrigin {
    /// Admitted by the live ingestion path — `POST /records` or
    /// `create_usage_records`.
    Live,
    /// Admitted by the dedicated bulk-import route — `POST /records/backfill`
    /// or `backfill_usage_records`.
    Backfill,
}

impl RecordOrigin {
    /// Every variant, so a guard over the closed label set cannot fall
    /// behind the enum. See [`EntryType::ALL`].
    pub const ALL: &'static [Self] = &[Self::Live, Self::Backfill];

    /// The wire spelling, shared by the REST projection, the `$filter`
    /// surface and the metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Backfill => "backfill",
        }
    }

    /// Every admissible wire label, derived from [`Self::ALL`]. See
    /// [`EntryType::wire_labels`].
    #[must_use]
    pub const fn wire_labels() -> &'static [&'static str] {
        const LABELS: [&str; RecordOrigin::ALL.len()] = {
            let mut labels = [""; RecordOrigin::ALL.len()];
            let mut i = 0;
            while i < RecordOrigin::ALL.len() {
                labels[i] = RecordOrigin::ALL[i].as_str();
                i += 1;
            }
            labels
        };
        &LABELS
    }
}

/// The withdrawal an invalidation entry carries: the entry it retracts and
/// why.
///
/// Grouping the two makes the half-shape unrepresentable; they are
/// both-or-neither
/// (`cpt-cf-usage-collector-adr-append-only-invalidation`), so no rule,
/// check or error for that case exists anywhere.
///
/// **The wire shape is two flat sibling properties, not a nested object.**
/// `usage-collector-v2.yaml` declares `invalidates` and `reason_code` side by
/// side; the serde shadows in the wire-codec section split and rejoin the
/// pair so the bytes are unchanged. A plugin author therefore implements
/// against one grouped field while reading an OAS that shows two
/// properties.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Invalidation {
    /// The accepted entry this one withdraws. Not an input to the derived
    /// identity; see [`UsageRecord::invalidation`].
    pub target: Uuid,
    /// Why the withdrawal was issued. Forbidden on an ordinary record —
    /// including one whose quantity is negative, which records real
    /// consumption rather than a correction.
    pub reason: ReasonCode,
}

/// Single usage record. The persisted shape is the canonical return value
/// of every create surface (new insert or silent idempotency replay).
///
/// A withdrawal's two halves are one field ([`Invalidation`]), so the pairing
/// is a property of this type rather than of a validation step; the shadow
/// struct that deserializes a wire body refuses the half-shape there. On the
/// wire they stay two flat sibling properties, both omitted on an ordinary
/// measurement.
// @cpt-dod:cpt-cf-usage-collector-entity-model:p1
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "UsageRecordWire")]
pub struct UsageRecord {
    /// Deterministic gateway-derived entry identity — see
    /// [`crate::v2::derive_usage_record_id`], which is normative for its inputs.
    /// Stamped on create and authoritative on read. Never caller-supplied:
    /// the create surface takes the identity-free [`CreateUsageRecord`].
    pub id: Uuid,
    /// Meter this record attaches to — the derived GTS type declaration
    /// (`gts.cf.core.uc.usage_record.v2~<segment>~`) resolved through
    /// `types-registry`, not a plugin-owned catalog row.
    pub gts_type_id: MeterTypeId,
    /// Owning tenant for this record. Caller-supplied; PDP uses it as the
    /// `OWNER_TENANT_ID` attribute.
    pub tenant_id: Uuid,
    /// Resource attribution composite (mandatory).
    pub resource_ref: ResourceRef,
    /// Optional subject attribution composite.
    pub subject_ref: Option<SubjectRef>,
    /// Caller-supplied metadata. Keys are validated [`MetadataKey`]s and
    /// values are typed as `String` end-to-end; closed-shape membership
    /// against the usage type's `metadata_fields` and the operator-configured
    /// size cap are enforced at the gateway before plugin dispatch. Omitted
    /// from the wire when empty.
    pub metadata: BTreeMap<MetadataKey, String>,
    /// The measured quantity, in the canonical unit of the entry's GTS type.
    /// Validated against the published range at construction and carried on
    /// the wire as a JSON string (see [`crate::v2::UsageQuantity`]). The sign is
    /// never constrained and carries no structural meaning: a negative
    /// quantity records real consumption, and an invalidation echoes the
    /// quantity it withdraws rather than negating it
    /// (`cpt-cf-usage-collector-adr-append-only-invalidation`).
    pub quantity: UsageQuantity,
    /// Mandatory caller-supplied key for at-least-once-with-dedup semantics,
    /// on both entry kinds: an invalidation repeats its target's (DESIGN
    /// §3.1, `IdempotencyKey`), which is how the gateway finds the entry
    /// being withdrawn. An identity input, so a single stable per-meter key
    /// covers many periods without collapsing them onto one entry.
    pub idempotency_key: IdempotencyKey,
    /// Gear-assigned instant of acceptance, stamped once per request by the
    /// Ingestion Gateway at microsecond precision. Never caller-supplied:
    /// [`CreateUsageRecord`] has no such field and its wire shadow refuses
    /// one. An absorbed retry reports the stored entry's instant, which is
    /// how a caller tells a replay from a first write. No identity input, and
    /// not compared on a dedup collision.
    pub accepted_at: time::OffsetDateTime,
    /// Which path admitted this entry — server-assigned by the Ingestion
    /// Gateway from the route it arrived on, never caller-supplied. See
    /// [`RecordOrigin`]. Not an input to [`Self::id`]'s derivation.
    pub origin: RecordOrigin,
    /// The withdrawal this entry carries, absent on an ordinary measurement.
    /// Its presence is what makes this entry an invalidation — hence
    /// [`Self::entry_type`], which reads it.
    ///
    /// [`Invalidation::target`] is no identity input, and server-assigned
    /// rather than caller-supplied: the gateway derives it from this entry's
    /// own identity inputs with `entry_type = record` and stamps it here.
    pub invalidation: Option<Invalidation>,
    /// Inclusive start of the emitter-supplied covered period (RFC 3339 on
    /// the wire) — the only emitter-supplied time attribution an entry
    /// carries. Persisted at microsecond precision, UTC-normalized by
    /// [`CreateUsageRecord::try_into_usage_record`].
    pub window_start: time::OffsetDateTime,
    /// Exclusive end of the covered period. At or after
    /// [`Self::window_start`]; equal bounds mark a point event, not an error.
    /// Enforced on both paths an entry arrives by: [`CreateUsageRecord`]'s
    /// projection, and `require_ordered_period` on deserialization.
    ///
    /// This is the bound every read path selects on; see
    /// [`crate::v2::TimeRange::contains_window_end`], which is normative for the
    /// rule.
    pub window_end: time::OffsetDateTime,
}

impl UsageRecord {
    /// This entry's kind, derived from the reference it carries.
    ///
    /// Not a field: a stored discriminator is a second place the kind can be
    /// read and the two can disagree
    /// (`cpt-cf-usage-collector-adr-append-only-invalidation`). Every
    /// surface that needs the value computes it here.
    #[must_use]
    pub const fn entry_type(&self) -> EntryType {
        if self.invalidation.is_some() {
            EntryType::Invalidation
        } else {
            EntryType::Record
        }
    }

    /// Whether `other` carries the same caller-supplied fields as `self`.
    ///
    /// This is the dedup comparison: two entries on one dedup identity are
    /// one entry when every field the caller supplied is equal, and a conflict
    /// otherwise. Most server-assigned fields are not compared: `id` (derived
    /// from the identity both sides already share), `accepted_at` (stamped
    /// afresh per request) and `origin` (the route, which a retry may change).
    ///
    /// The exception, an invalidation's [`Invalidation::target`], **is**
    /// compared, because it travels grouped with the caller's `reason_code`
    /// in one field. Both sides reaching this comparison share the identity
    /// the target is derived from, so what the grouped field really decides
    /// here is `reason_code`.
    ///
    /// Both sides are destructured, so a field added to [`UsageRecord`] fails
    /// to compile here until it is classified.
    #[must_use]
    pub fn caller_supplied_eq(&self, other: &Self) -> bool {
        let Self {
            id: _,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at: _,
            origin: _,
            invalidation,
            window_start,
            window_end,
        } = self;
        let Self {
            id: _,
            gts_type_id: other_gts_type_id,
            tenant_id: other_tenant_id,
            resource_ref: other_resource_ref,
            subject_ref: other_subject_ref,
            metadata: other_metadata,
            quantity: other_quantity,
            idempotency_key: other_idempotency_key,
            accepted_at: _,
            origin: _,
            invalidation: other_invalidation,
            window_start: other_window_start,
            window_end: other_window_end,
        } = other;
        gts_type_id == other_gts_type_id
            && tenant_id == other_tenant_id
            && resource_ref == other_resource_ref
            && subject_ref == other_subject_ref
            && metadata == other_metadata
            && quantity == other_quantity
            && idempotency_key == other_idempotency_key
            && invalidation == other_invalidation
            && window_start == other_window_start
            && window_end == other_window_end
    }
}

/// Identity-free create submission — the input to every create surface
/// ([`crate::v2::UsageCollectorClientV2::create_usage_records`]).
///
/// This mirrors [`UsageRecord`] with [`Self::entry_type`] added — the
/// persisted shape derives its kind, this one is told it — and minus the
/// fields the server assigns: `id`, `accepted_at`, `origin`, and, on an
/// invalidation, the target the gateway resolves. The wire REST surface
/// encodes the same shape as `CreateUsageRecordRequest`.
///
/// One shape carries both entry kinds and [`Self::entry_type`] alone decides
/// which. `reason_code` is the caller's second statement about that same kind
/// rather than the kind itself: required on an `invalidation`, forbidden on a
/// `record`, and both projections refuse a submission whose two statements
/// disagree.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "CreateUsageRecordWire")]
pub struct CreateUsageRecord {
    /// The kind this submission declares — caller-supplied, required, and
    /// the sixth input to the derived identity.
    ///
    /// **Never inferred.** It is the one field that keeps a withdrawal's
    /// identity apart from its target's: the faithful copy leaves every other
    /// identity input equal to the target's, so a submission that omitted
    /// this would derive the target's own `id` and be absorbed as a retry.
    ///
    /// It must agree with [`Self::invalidation`], and the projections enforce
    /// that rather than the type.
    pub entry_type: EntryType,
    /// Meter this record attaches to — the derived GTS type declaration
    /// (`gts.cf.core.uc.usage_record.v2~<segment>~`) resolved through
    /// `types-registry`, not a plugin-owned catalog row.
    pub gts_type_id: MeterTypeId,
    /// Owning tenant for this record. Caller-supplied; PDP uses it as the
    /// `OWNER_TENANT_ID` attribute.
    pub tenant_id: Uuid,
    /// Resource attribution composite (mandatory).
    pub resource_ref: ResourceRef,
    /// Optional subject attribution composite.
    pub subject_ref: Option<SubjectRef>,
    /// Caller-supplied metadata. Same validation and closed-shape rules as
    /// [`UsageRecord::metadata`]. Omitted from the wire when empty.
    pub metadata: BTreeMap<MetadataKey, String>,
    /// The measured quantity. Same encoding and sign rules as [`UsageRecord::quantity`].
    pub quantity: UsageQuantity,
    /// The caller's key, required on **both** entry kinds: a withdrawal
    /// repeats its target's, which is what lets the gateway find the target
    /// from the submission alone.
    ///
    /// Not `Option`: the published schema lists it in `required` on both
    /// `oneOf` branches, so an absent key is refused by the codec rather
    /// than carried to a projection to report.
    pub idempotency_key: IdempotencyKey,
    /// The withdrawal's stated reason, present exactly when this submission
    /// is an invalidation.
    ///
    /// **No target.** `invalidates` is server-assigned: the gateway derives
    /// the target's identifier from this submission's own identity inputs
    /// with
    /// `entry_type = record`, looks it up converged-only under the PDP scope,
    /// and stamps the result. A caller-supplied target would let an emitter
    /// name a record it never measured.
    pub invalidation: Option<ReasonCode>,
    /// Inclusive start of the covered period this submission measures (RFC
    /// 3339 on the wire; the codec requires an offset, so an offset-less
    /// timestamp never reaches the projection). An identity input, so it
    /// feeds the derived `id`.
    pub window_start: time::OffsetDateTime,
    /// Exclusive end of the covered period. Must be at or after
    /// [`Self::window_start`]; equal bounds submit a point event. An identity
    /// input, which is why an emitter that recomputes its bounds on retry
    /// derives a different identifier, and why deterministic bounds are an
    /// emitter obligation.
    pub window_end: time::OffsetDateTime,
}

impl CreateUsageRecord {
    /// The kind this submission declares — [`Self::entry_type`], read back.
    ///
    /// Unlike [`UsageRecord::entry_type`] this reads a stored field rather
    /// than projecting one; an accessor all the same, so asking a submission
    /// its kind reads the same way as asking an accepted entry.
    #[must_use]
    pub const fn entry_type(&self) -> EntryType {
        self.entry_type
    }

    /// Refuses a submission whose two statements about its own kind
    /// disagree: `reason_code` is required when `entry_type` is
    /// `invalidation` and MUST NOT appear on a `record`.
    ///
    /// Both halves are caller-supplied, so a disagreement is an emitter error
    /// — [`UsageCollectorError::InvalidArgument`] naming `reason_code`, not
    /// the [`UsageCollectorError::Internal`] the projections raise when the
    /// submission is consistent and the *projection* is the wrong one for it.
    ///
    /// Both projections run this **before** their own guard, so a submission
    /// declaring `entry_type: invalidation` with no reason code is reported as
    /// an emitter error rather than a host-contract breach.
    fn require_agreeing_kind(&self) -> Result<(), UsageCollectorError> {
        match (self.entry_type, self.invalidation.is_some()) {
            (EntryType::Invalidation, false) => {
                Err(UsageCollectorError::reason_code_required_on_invalidation())
            }
            (EntryType::Record, true) => {
                Err(UsageCollectorError::reason_code_forbidden_on_record())
            }
            (EntryType::Record, false) | (EntryType::Invalidation, true) => Ok(()),
        }
    }

    /// Projects an ordinary measurement into the persisted [`UsageRecord`]
    /// shape, validating the submission's own shape first.
    ///
    /// Both period preconditions are rejected **before** the derivation runs,
    /// so the projection is fallible rather than the caller's obligation.
    ///
    /// In order: `entry_type` and `reason_code` must agree and the declared
    /// kind must be `record`; both bounds are normalized to UTC; each must
    /// then carry at most microsecond precision; the period must be ordered
    /// (`window_start <= window_end`, equal bounds being a point event); and
    /// only then is `id` derived
    /// ([`crate::v2::id::derive_usage_record_id`], with `entry_type = record`).
    /// Every other field is forwarded verbatim; `origin` and `accepted_at`
    /// are server-assigned and stamped from the arguments.
    ///
    /// Neither period precondition truncates. A truncated bound would be
    /// persisted under an `id` derived from the truncated value while the
    /// emitter reproduces the id it submitted.
    ///
    /// The rules an invalidation must satisfy against its *target* need a
    /// lookup and belong to the ingestion gateway.
    ///
    /// # Errors
    ///
    /// [`UsageCollectorError::InvalidArgument`] when the submission's
    /// `entry_type` and `reason_code` disagree, when a bound is finer than
    /// microsecond precision, or when the period is inverted — all emitter
    /// errors.
    ///
    /// [`UsageCollectorError::Internal`] when a self-consistent submission
    /// declares `entry_type: invalidation`: it is well-formed and simply not
    /// this projection's, needing a target only the gateway can resolve, so
    /// arriving here means this crate's caller chose wrongly. See
    /// [`UsageCollectorError::withdrawal_needs_its_target`].
    pub fn try_into_usage_record(
        self,
        origin: RecordOrigin,
        accepted_at: time::OffsetDateTime,
    ) -> Result<UsageRecord, UsageCollectorError> {
        self.require_agreeing_kind()?;
        // Keyed on the declared `entry_type`, not on `invalidation.is_some()`:
        // keyed on the reason code, a `record` that states one would satisfy
        // this guard and its classification would depend on check order.
        if matches!(self.entry_type, EntryType::Invalidation) {
            return Err(UsageCollectorError::withdrawal_needs_its_target());
        }
        self.project(origin, accepted_at, None)
    }

    /// Projects a withdrawal, stamping the target the gateway resolved.
    ///
    /// `target` is the identifier the gateway resolved for the entry being
    /// withdrawn — [`crate::v2::derive_usage_record_id`] over this submission's
    /// own identity inputs with [`EntryType::Record`], looked up
    /// converged-only under the PDP scope. It is not re-derived here, so an
    /// unresolvable target has already been refused by the time this is
    /// called.
    ///
    /// Validation and derivation are otherwise [`Self::try_into_usage_record`]'s,
    /// except that `id` is derived with `entry_type = invalidation` — the one
    /// input keeping this entry's identity apart from its target's.
    ///
    /// # Errors
    ///
    /// [`UsageCollectorError::InvalidArgument`] on the same grounds as
    /// [`Self::try_into_usage_record`] — including a submission that
    /// declares `entry_type: invalidation` and states no reason code.
    ///
    /// [`UsageCollectorError::Internal`] when a self-consistent submission
    /// declares `entry_type: record`, which is this projection's mirror of
    /// the misuse above. See [`UsageCollectorError::missing_reason_code`].
    pub fn try_into_invalidation_record(
        mut self,
        origin: RecordOrigin,
        accepted_at: time::OffsetDateTime,
        target: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError> {
        self.require_agreeing_kind()?;
        let Some(reason) = self.invalidation.take() else {
            return Err(UsageCollectorError::missing_reason_code());
        };
        self.project(origin, accepted_at, Some(Invalidation { target, reason }))
    }

    /// The half both projections share: UTC normalization, the two period
    /// preconditions, and the derivation over the identity inputs.
    ///
    /// The digest's kind input is the caller's own [`Self::entry_type`];
    /// `invalidation` is the resolved withdrawal the projected entry carries.
    /// Unreachable from either caller above, which run `require_agreeing_kind`
    /// and then refuse the kind that is not their own, but a disagreement is
    /// refused here rather than assumed away: see the guard below.
    ///
    /// `self` is destructured rather than read field by field, so a field
    /// added to [`CreateUsageRecord`] fails to compile here until it is
    /// carried onto the projected entry instead of being silently dropped.
    fn project(
        self,
        origin: RecordOrigin,
        accepted_at: time::OffsetDateTime,
        invalidation: Option<Invalidation>,
    ) -> Result<UsageRecord, UsageCollectorError> {
        // Refused rather than asserted: a `debug_assert` is compiled out of
        // the release build that would then mint an identifier under the
        // wrong entry type, and silently drop the caller's reason code with
        // it.
        match (self.entry_type, invalidation.is_some()) {
            (EntryType::Invalidation, false) => {
                return Err(UsageCollectorError::withdrawal_needs_its_target());
            }
            (EntryType::Record, true) => {
                return Err(UsageCollectorError::missing_reason_code());
            }
            (EntryType::Record, false) | (EntryType::Invalidation, true) => {}
        }

        let Self {
            // Already checked by the caller against the reason code: the kind
            // input below reads this value while the projected entry carries
            // the `invalidation` argument.
            entry_type,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            // Always `None` by the time this runs: one caller refuses a
            // submission declaring `entry_type: invalidation`, the other takes
            // the reason out and hands it back as the `invalidation` argument.
            invalidation: _,
            window_start: submitted_start,
            window_end: submitted_end,
        } = self;

        // Normalization runs before the preconditions: `UtcOffset` holds whole
        // seconds, so the nanosecond component is invariant under the
        // conversion, and it keeps an offset with non-zero seconds out of
        // `crate::v2::error`'s RFC 3339 formatter, which cannot render one.
        let window_start = submitted_start.to_offset(time::UtcOffset::UTC);
        let window_end = submitted_end.to_offset(time::UtcOffset::UTC);

        reject_nul_metadata_values(&metadata)?;

        require_microsecond_precision(WINDOW_START_FIELD, window_start)?;
        require_microsecond_precision(WINDOW_END_FIELD, window_end)?;

        if window_end < window_start {
            return Err(UsageCollectorError::inverted_covered_period(
                window_start,
                window_end,
            ));
        }

        // The inversion is refused above, so `?` carries only an out-of-range
        // bound here — unreachable on the RFC 3339-parsed wire path.
        let id = crate::v2::id::derive_usage_record_id(
            tenant_id,
            &gts_type_id,
            &idempotency_key,
            window_start,
            window_end,
            entry_type,
        )?;

        Ok(UsageRecord {
            id,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            // Declaration order, which
            // `clippy::inconsistent_struct_constructor` requires.
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        })
    }
}

/// Refuses a metadata value carrying a NUL.
///
/// Values are domain-opaque and otherwise unconstrained, but metadata reaches
/// storage as a `jsonb` document and no `jsonb` string can carry a NUL — so an
/// ingested one fails inside the plugin as an `Internal`, a 500 the caller
/// provoked. Refused here it is a 400 instead. [`MetadataKey::new`] excludes
/// the whole ASCII control range on the key for a wider reason.
fn reject_nul_metadata_values(
    metadata: &BTreeMap<MetadataKey, String>,
) -> Result<(), UsageCollectorError> {
    for (key, value) in metadata {
        if value.contains('\0') {
            return Err(UsageCollectorError::metadata_value_carries_nul(key));
        }
    }
    Ok(())
}

/// Rejects a covered-period bound carrying finer than microsecond
/// precision.
///
/// The microsecond is the precision ceiling of the identity derivation, whose
/// canonical bound form is a six-digit fraction, so a finer value has no
/// representation there. Both halves of that ceiling refuse a finer bound
/// rather than truncating it: this one as a typed field violation, and
/// [`crate::v2::canonical_period_bound`] as
/// [`PeriodBoundInvalid::SubMicrosecond`](crate::v2::PeriodBoundInvalid::SubMicrosecond).
/// A leap second lands here too, with no branch of its own: `time`'s
/// RFC 3339 parser renders a `:60` second as `59.999999999`, and
/// [`time::Time`] cannot represent second 60 at all.
fn require_microsecond_precision(
    field: &'static str,
    bound: time::OffsetDateTime,
) -> Result<(), UsageCollectorError> {
    if bound.nanosecond().is_multiple_of(1_000) {
        Ok(())
    } else {
        Err(UsageCollectorError::sub_microsecond_period_bound(
            field, bound,
        ))
    }
}

// Wire codecs
//
// Nothing below is public API: the shadows are private and the two
// `Serialize` impls are the visible behaviour of the domain types themselves.

/// `skip_serializing_if` predicate for a borrowed metadata map. The attribute
/// hands the field by reference, so a borrowed field arrives as `&&BTreeMap`
/// and `BTreeMap::is_empty` does not typecheck against it. The double
/// reference is the attribute's calling convention — hence the allow.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn metadata_is_empty(metadata: &&BTreeMap<MetadataKey, String>) -> bool {
    metadata.is_empty()
}

/// Splits an [`Invalidation`] into the two flat wire properties, or refuses
/// a half-shape.
///
/// The persisted shape's shadow alone: the ingestion shape's shadow refuses a
/// submitted `invalidates` as an unknown field, leaving no half-shape there.
///
/// The refusal is a plain message rather than a typed
/// [`UsageCollectorError`]: it fires inside a `Deserialize`, which erases
/// everything but the string.
fn invalidation_from_wire(
    invalidates: Option<Uuid>,
    reason_code: Option<ReasonCode>,
) -> Result<Option<Invalidation>, String> {
    match (invalidates, reason_code) {
        (None, None) => Ok(None),
        (Some(target), Some(reason)) => Ok(Some(Invalidation { target, reason })),
        (Some(_), None) => Err(
            "`invalidates` and `reason_code` are both-or-neither on a usage \
             record; `reason_code` is missing"
                .to_owned(),
        ),
        (None, Some(_)) => Err(
            "`invalidates` and `reason_code` are both-or-neither on a usage \
             record; `invalidates` is missing"
                .to_owned(),
        ),
    }
}

/// Owned deserialization shadow for [`UsageRecord`].
///
/// Exists because the pair is flat on the wire and grouped in the type, and
/// `#[serde(flatten)]` — the obvious way to bridge that — cannot be used
/// under `#[serde(deny_unknown_fields)]`. The `deny_unknown_fields` lives
/// here, so a submitted `entry_type` (or any other stray key) is still
/// refused.
///
/// Each entry shape has an owned shadow for reading and a borrowing one for
/// writing, which splits each field's serde attributes between the two; only
/// `models_tests`' round-trip makes the halves agree.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageRecordWire {
    id: Uuid,
    gts_type_id: MeterTypeId,
    tenant_id: Uuid,
    resource_ref: ResourceRef,
    #[serde(default)]
    subject_ref: Option<SubjectRef>,
    #[serde(default)]
    metadata: BTreeMap<MetadataKey, String>,
    quantity: UsageQuantity,
    idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    accepted_at: time::OffsetDateTime,
    origin: RecordOrigin,
    #[serde(default)]
    invalidates: Option<Uuid>,
    #[serde(default)]
    reason_code: Option<ReasonCode>,
    #[serde(with = "time::serde::rfc3339")]
    window_start: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: time::OffsetDateTime,
}

impl TryFrom<UsageRecordWire> for UsageRecord {
    type Error = String;

    fn try_from(wire: UsageRecordWire) -> Result<Self, Self::Error> {
        let UsageRecordWire {
            id,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidates,
            reason_code,
            window_start,
            window_end,
        } = wire;
        require_ordered_period(window_start, window_end)?;
        Ok(Self {
            id,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key: IdempotencyKey::from_stored(idempotency_key)
                .map_err(|e| e.to_string())?,
            accepted_at,
            origin,
            invalidation: invalidation_from_wire(invalidates, reason_code)?,
            window_start,
            window_end,
        })
    }
}

/// Refuses a covered period whose end precedes its start. Equal bounds are a
/// point event, as they are at projection.
///
/// The rule [`CreateUsageRecord::project`] enforces at ingestion, applied
/// where an already-shaped entry is read back — a stored row or a
/// plugin-built payload — so [`UsageRecord::window_end`]'s doc is an
/// invariant on both paths rather than on one. The refusal is a plain
/// message for the reason [`invalidation_from_wire`] gives.
fn require_ordered_period(
    window_start: time::OffsetDateTime,
    window_end: time::OffsetDateTime,
) -> Result<(), String> {
    if window_end < window_start {
        return Err(format!(
            "`window_end` is the exclusive end of the covered period and is at or after \
             `window_start`; got window_start={window_start}, window_end={window_end}"
        ));
    }
    Ok(())
}

/// Borrowing serialization shadow for [`UsageRecord`].
///
/// Borrowed rather than owned so serializing a record costs no clone of its
/// metadata map and strings — which `#[serde(into = "…")]` would charge on
/// every call. The [`Serialize`] impl below destructures the record
/// exhaustively, so a field added to [`UsageRecord`] and not to this shadow
/// is a compile error rather than a key that silently stops being emitted.
#[derive(Serialize)]
struct UsageRecordWireRef<'a> {
    id: Uuid,
    gts_type_id: &'a MeterTypeId,
    tenant_id: Uuid,
    resource_ref: &'a ResourceRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_ref: Option<&'a SubjectRef>,
    #[serde(skip_serializing_if = "metadata_is_empty")]
    metadata: &'a BTreeMap<MetadataKey, String>,
    quantity: UsageQuantity,
    idempotency_key: &'a IdempotencyKey,
    #[serde(with = "time::serde::rfc3339")]
    accepted_at: time::OffsetDateTime,
    origin: RecordOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalidates: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<&'a ReasonCode>,
    #[serde(with = "time::serde::rfc3339")]
    window_start: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: time::OffsetDateTime,
}

impl Serialize for UsageRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Self {
            id,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        } = self;
        UsageRecordWireRef {
            id: *id,
            gts_type_id,
            tenant_id: *tenant_id,
            resource_ref,
            subject_ref: subject_ref.as_ref(),
            metadata,
            quantity: *quantity,
            idempotency_key,
            accepted_at: *accepted_at,
            origin: *origin,
            invalidates: invalidation.as_ref().map(|i| i.target),
            reason_code: invalidation.as_ref().map(|i| &i.reason),
            window_start: *window_start,
            window_end: *window_end,
        }
        .serialize(serializer)
    }
}

/// Owned deserialization shadow for [`CreateUsageRecord`]. See
/// [`UsageRecordWire`] for why the shadow exists; this is the ingestion
/// half.
///
/// It is **not** on the REST path: a request body deserializes into the host
/// crate's own `CreateUsageRecordRequest` DTO. Both paths must agree about the
/// ingestion key set, and neither can check the other.
///
/// It declares no `invalidates` — that field is server-assigned, and
/// `deny_unknown_fields` above is what rejects a submitted one.
///
/// It **does** declare `entry_type`, with no `serde(default)`: the published
/// schema lists it among the required properties, so a body omitting it is
/// refused as a missing field rather than read as a `record`. Its agreement
/// with `reason_code` is checked by the projections, which can raise a typed
/// field violation where a codec could only raise a plain serde string.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUsageRecordWire {
    entry_type: EntryType,
    gts_type_id: MeterTypeId,
    tenant_id: Uuid,
    resource_ref: ResourceRef,
    #[serde(default)]
    subject_ref: Option<SubjectRef>,
    #[serde(default)]
    metadata: BTreeMap<MetadataKey, String>,
    quantity: UsageQuantity,
    idempotency_key: IdempotencyKey,
    #[serde(default)]
    reason_code: Option<ReasonCode>,
    #[serde(with = "time::serde::rfc3339")]
    window_start: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: time::OffsetDateTime,
}

impl TryFrom<CreateUsageRecordWire> for CreateUsageRecord {
    type Error = String;

    fn try_from(wire: CreateUsageRecordWire) -> Result<Self, Self::Error> {
        let CreateUsageRecordWire {
            entry_type,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            reason_code,
            window_start,
            window_end,
        } = wire;
        Ok(Self {
            entry_type,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            invalidation: reason_code,
            window_start,
            window_end,
        })
    }
}

/// Borrowing serialization shadow for [`CreateUsageRecord`]. See
/// [`UsageRecordWireRef`] for why it borrows.
#[derive(Serialize)]
struct CreateUsageRecordWireRef<'a> {
    entry_type: EntryType,
    gts_type_id: &'a MeterTypeId,
    tenant_id: Uuid,
    resource_ref: &'a ResourceRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_ref: Option<&'a SubjectRef>,
    #[serde(skip_serializing_if = "metadata_is_empty")]
    metadata: &'a BTreeMap<MetadataKey, String>,
    quantity: UsageQuantity,
    idempotency_key: &'a IdempotencyKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<&'a ReasonCode>,
    #[serde(with = "time::serde::rfc3339")]
    window_start: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: time::OffsetDateTime,
}

impl Serialize for CreateUsageRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Self {
            entry_type,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            invalidation,
            window_start,
            window_end,
        } = self;
        CreateUsageRecordWireRef {
            entry_type: *entry_type,
            gts_type_id,
            tenant_id: *tenant_id,
            resource_ref,
            subject_ref: subject_ref.as_ref(),
            metadata,
            quantity: *quantity,
            idempotency_key,
            reason_code: invalidation.as_ref(),
            window_start: *window_start,
            window_end: *window_end,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
