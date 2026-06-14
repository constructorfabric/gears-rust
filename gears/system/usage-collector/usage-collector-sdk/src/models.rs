//! Foundation domain models for the Usage Collector SDK.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

use gts::{GtsID, GtsInstanceId};

use crate::error::UsageCollectorError;

// ---------------------------------------------------------------------------
// Usage-kind discriminator
// ---------------------------------------------------------------------------

/// Closed classification axis for usage types.
///
/// `Counter` and `Gauge` are CF-platform-internal kinds with no vendor
/// extensibility. Serde `deny_unknown_fields` on [`UsageType`] plus the
/// closed-enum serde shape rejects any other value at the deserialize
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageKind {
    /// Append-only counter semantics. Compensations (negative deltas with
    /// `corrects_id` set) are accepted.
    Counter,
    /// Snapshot-overwrite semantics. Compensations are rejected; the only
    /// correction for a gauge is deactivation.
    Gauge,
}

impl std::str::FromStr for UsageKind {
    type Err = UsageCollectorError;

    /// Delegates to the serde deserializer so the wire-string → variant
    /// mapping has a single source of truth (`#[serde(rename_all = ...)]`
    /// on the enum). The serde error is rewrapped as
    /// [`UsageCollectorError::InvalidUsageKind`] to preserve the canonical
    /// `Problem` envelope at REST boundaries.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_owned()))
            .map_err(|_| UsageCollectorError::InvalidUsageKind { raw: s.to_owned() })
    }
}

// ---------------------------------------------------------------------------
// MetadataKey
// ---------------------------------------------------------------------------

/// Validating newtype over a metadata key string.
///
/// Every site in the SDK that names a declared metadata key — the keys in
/// [`UsageType::metadata_fields`], the keys of [`UsageRecord::metadata`],
/// the key of a [`MetadataFilter`], and the payload of
/// [`AggregationDimension::Metadata`] — carries this type rather than a bare
/// `String`, so a malformed key cannot reach any consumer past the SDK
/// boundary.
///
/// Validation rules are intentionally minimal: keys are domain-opaque
/// (operators choose them) so the SDK refuses to encode casing or charset
/// policy.
///
/// Closed-shape membership — every key on a record MUST be in the referenced
/// usage type's `metadata_fields` — remains a gateway-time check; it cannot
/// be expressed at the type level without the catalog context, and the
/// gateway is its single owner.
///
/// # Validation
///
/// - Non-empty.
/// - No NUL bytes (Postgres `jsonb` key requirement).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MetadataKey(String);

impl MetadataKey {
    /// Creates a [`MetadataKey`] after validating the value is non-empty and
    /// contains no NUL bytes.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidMetadataKey`] when the input is
    /// empty or contains a NUL byte.
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        if raw.is_empty() {
            return Err(UsageCollectorError::InvalidMetadataKey {
                reason: "metadata key must not be empty".into(),
            });
        }
        if raw.contains('\0') {
            return Err(UsageCollectorError::InvalidMetadataKey {
                reason: "metadata key must not contain NUL bytes".into(),
            });
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

// ---------------------------------------------------------------------------
// Attribution composites
// ---------------------------------------------------------------------------

/// Reference to the resource instance to which usage is attributed.
/// Mandatory on every usage record.
///
/// Both `resource_id` and `resource_type` are validated non-empty and
/// NUL-byte-free at construction (Postgres `text` column requirement);
/// `Deserialize` routes through [`Self::new`] so wire payloads cannot
/// bypass the invariant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ResourceRef {
    /// Resource instance identifier inside the attributed tenant scope.
    resource_id: String,
    /// Type discriminator such as `compute.vm`.
    resource_type: String,
}

impl ResourceRef {
    /// Creates a [`ResourceRef`] after validating both components are
    /// non-empty and contain no NUL bytes.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidResourceRef`] when `resource_id`
    /// or `resource_type` is empty or contains a NUL byte.
    pub fn new(
        resource_id: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Result<Self, UsageCollectorError> {
        let resource_id = resource_id.into();
        if resource_id.is_empty() {
            return Err(UsageCollectorError::InvalidResourceRef {
                reason: "resource_id must not be empty".into(),
            });
        }
        if resource_id.contains('\0') {
            return Err(UsageCollectorError::InvalidResourceRef {
                reason: "resource_id must not contain NUL bytes".into(),
            });
        }
        let resource_type = resource_type.into();
        if resource_type.is_empty() {
            return Err(UsageCollectorError::InvalidResourceRef {
                reason: "resource_type must not be empty".into(),
            });
        }
        if resource_type.contains('\0') {
            return Err(UsageCollectorError::InvalidResourceRef {
                reason: "resource_type must not contain NUL bytes".into(),
            });
        }
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
/// `subject_id` is validated non-empty and NUL-byte-free; `subject_type`,
/// when supplied, is validated non-empty and NUL-byte-free (an explicit
/// `Some("")` is rejected). The NUL restriction matches the Postgres
/// `text` column requirement. `Deserialize` routes through [`Self::new`]
/// so wire payloads cannot bypass either invariant.
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
    /// also validated to contain no NUL bytes.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidSubjectRef`] when `subject_id`
    /// is empty, `subject_type` is `Some("")`, or either component contains
    /// a NUL byte.
    pub fn new(
        subject_id: impl Into<String>,
        subject_type: Option<impl Into<String>>,
    ) -> Result<Self, UsageCollectorError> {
        let subject_id = subject_id.into();
        if subject_id.is_empty() {
            return Err(UsageCollectorError::InvalidSubjectRef {
                reason: "subject_id must not be empty".into(),
            });
        }
        if subject_id.contains('\0') {
            return Err(UsageCollectorError::InvalidSubjectRef {
                reason: "subject_id must not contain NUL bytes".into(),
            });
        }
        let subject_type = match subject_type {
            None => None,
            Some(s) => {
                let s = s.into();
                if s.is_empty() {
                    return Err(UsageCollectorError::InvalidSubjectRef {
                        reason: "subject_type must not be empty when supplied".into(),
                    });
                }
                if s.contains('\0') {
                    return Err(UsageCollectorError::InvalidSubjectRef {
                        reason: "subject_type must not contain NUL bytes".into(),
                    });
                }
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

// ---------------------------------------------------------------------------
// Idempotency key
// ---------------------------------------------------------------------------

/// Validating newtype over the caller-supplied idempotency key string.
///
/// Every [`UsageRecord::idempotency_key`] carries this type rather than a
/// bare `String`. The plugin SPI dedups on
/// `(tenant_id, usage_type_gts_id, idempotency_key)` per `plugin-spi.md`,
/// and the key is declared mandatory on every record — the newtype
/// enforces that "mandatory" at the type level so an SDK consumer cannot
/// build a record with an empty key.
///
/// # Validation
///
/// - Non-empty.
/// - No NUL bytes (Postgres `text` column requirement).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Creates an [`IdempotencyKey`] after validating the value is non-empty
    /// and contains no NUL bytes.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidIdempotencyKey`] when the input
    /// is empty or contains a NUL byte.
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        if raw.is_empty() {
            return Err(UsageCollectorError::InvalidIdempotencyKey {
                reason: "idempotency_key must not be empty".into(),
            });
        }
        if raw.contains('\0') {
            return Err(UsageCollectorError::InvalidIdempotencyKey {
                reason: "idempotency_key must not contain NUL bytes".into(),
            });
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

// ---------------------------------------------------------------------------
// UsageType identity
// ---------------------------------------------------------------------------

/// Deployment-unique usage-type human identifier.
///
/// Validating newtype over [`gts::GtsInstanceId`]: the id MUST derive from
/// the reserved abstract base [`Self::USAGE_RECORD_BASE`] with at least one
/// further `~`-separated segment. Counter / gauge classification is carried
/// separately by [`UsageType::kind`]; the id does not encode kind.
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-principle-semantics-enforcement:p2
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct UsageTypeGtsId(GtsInstanceId);

impl UsageTypeGtsId {
    /// Reserved abstract base type id for usage records.
    ///
    /// Every catalog `gts_id` MUST left-prefix-match this value and carry at
    /// least one further `~`-separated derivation segment. The base itself
    /// is abstract and rejected as a bare value.
    pub const USAGE_RECORD_BASE: &'static str = "gts.cf.core.uc.usage_record.v1~";

    /// Creates a `UsageTypeGtsId` after validating that the input is a
    /// well-formed GTS instance id deriving from [`Self::USAGE_RECORD_BASE`].
    ///
    /// Validation routes through [`gts::GtsID::new`], which enforces the GTS
    /// per-segment grammar (`vendor.package.namespace.type.v<major>[.<minor>]`),
    /// the allowed character set, terminator semantics, and the chained-id
    /// rules. That is the same validator other gears use for catalog-key
    /// GTS strings (e.g. `account-management::create_tenant`), so a
    /// malformed id surfaces with the canonical GTS error chain instead of a
    /// raw `strip_prefix` miss.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidUsageTypeGtsId`] when the input
    /// is not a syntactically valid GTS id, is a GTS *type* id (trailing
    /// `~`) rather than an instance id, or does not derive from
    /// [`Self::USAGE_RECORD_BASE`] (the base must appear as the first
    /// segment of the parsed chain).
    pub fn new(value: impl Into<String>) -> Result<Self, UsageCollectorError> {
        let raw = value.into();
        let parsed = GtsID::new(&raw).map_err(|e| UsageCollectorError::InvalidUsageTypeGtsId {
            raw: raw.clone(),
            reason: format!("usage type gts_id `{raw}` is not a valid GTS id: {e}"),
        })?;
        if parsed.is_type() {
            return Err(UsageCollectorError::InvalidUsageTypeGtsId {
                raw: raw.clone(),
                reason: format!(
                    "usage type gts_id `{raw}` must be a GTS instance id (no trailing `~`), \
                     not a type id"
                ),
            });
        }
        // Parent-chain match at GTS-segment granularity (not byte
        // granularity). `get_type_id()` returns the prefix up to and
        // including the last `~`, which for the canonical
        // `base~concrete` shape is exactly `USAGE_RECORD_BASE`. Any other
        // base, or a deeper chain whose immediate parent is not the
        // usage-record base, fails this check — only direct derivation
        // from the reserved base is admitted into the catalog.
        if parsed.get_type_id().as_deref() != Some(Self::USAGE_RECORD_BASE) {
            return Err(UsageCollectorError::InvalidUsageTypeGtsId {
                raw: raw.clone(),
                reason: format!(
                    "usage type gts_id `{raw}` must derive from the reserved base `{base}`",
                    base = Self::USAGE_RECORD_BASE,
                ),
            });
        }
        // The last parsed segment is the derivation tail. The `let Some`
        // fall-through is structurally unreachable — `get_type_id()`
        // returning `Some(base)` implies `gts_id_segments.len() >= 2` —
        // but is kept as a graceful error rather than `expect` to satisfy
        // the workspace `clippy::expect_used` rule.
        let Some(segment) = parsed.gts_id_segments.last().map(|s| s.segment.as_str()) else {
            return Err(UsageCollectorError::InvalidUsageTypeGtsId {
                raw: raw.clone(),
                reason: format!("usage type gts_id `{raw}` is missing a derivation segment"),
            });
        };
        Ok(Self(GtsInstanceId::new(Self::USAGE_RECORD_BASE, segment)))
    }

    /// Borrows the validated GTS instance id.
    #[must_use]
    pub fn as_instance_id(&self) -> &GtsInstanceId {
        &self.0
    }
}

impl AsRef<str> for UsageTypeGtsId {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl std::fmt::Display for UsageTypeGtsId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

impl PartialOrd for UsageTypeGtsId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UsageTypeGtsId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_ref().cmp(other.0.as_ref())
    }
}

impl<'de> Deserialize<'de> for UsageTypeGtsId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        UsageTypeGtsId::new(raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// UsageType catalog row
// ---------------------------------------------------------------------------

/// Usage-type catalog row exchanged across SDK, plugin SPI, and REST surfaces.
///
/// The row carries `gts_id`, the closed `kind: UsageKind` discriminator
/// (counter vs gauge), and the closed `metadata_fields` list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageType {
    /// Catalog primary key; `usage_records.gts_id` references this value.
    pub gts_id: UsageTypeGtsId,
    /// Counter / gauge classification. Serde `deny_unknown_fields` plus the
    /// closed-enum shape rejects any other value at the deserialize boundary.
    pub kind: UsageKind,
    /// Closed set of declared metadata keys. Every key on a record's
    /// `metadata` map MUST be a member; values are typed as `String`;
    /// undeclared keys are rejected at the gateway. Each key is a
    /// validated [`MetadataKey`] (non-empty, no NUL bytes) so malformed
    /// declarations cannot land here, and the field is deserialized
    /// through [`deserialize_metadata_fields`] so duplicate keys are
    /// rejected at the wire boundary instead of silently collapsing.
    #[serde(deserialize_with = "deserialize_metadata_fields")]
    pub metadata_fields: BTreeSet<MetadataKey>,
}

/// Deserialize `metadata_fields` through a `Vec<MetadataKey>` so the SDK
/// boundary rejects duplicate keys instead of silently collapsing them
/// into the `BTreeSet`. The REST DTO path additionally surfaces the
/// typed [`UsageCollectorError::DuplicateMetadataField`] via
/// `metadata_fields_from_wire`; this function provides the same
/// duplicate-rejection guarantee for any non-REST wire entry point
/// (config loader, alternate IPC, plugin SPI replay).
fn deserialize_metadata_fields<'de, D>(d: D) -> Result<BTreeSet<MetadataKey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<MetadataKey>::deserialize(d)?;
    let mut set = BTreeSet::new();
    for (index, key) in raw.into_iter().enumerate() {
        if !set.insert(key) {
            return Err(serde::de::Error::custom(format!(
                "duplicate metadata field at index {index}"
            )));
        }
    }
    Ok(set)
}

impl UsageType {
    /// `true` when this usage type carries counter semantics.
    #[must_use]
    pub fn is_counter(&self) -> bool {
        matches!(self.kind, UsageKind::Counter)
    }

    /// `true` when this usage type carries gauge semantics.
    #[must_use]
    pub fn is_gauge(&self) -> bool {
        matches!(self.kind, UsageKind::Gauge)
    }
}

// ---------------------------------------------------------------------------
// Usage-record exchange types
// ---------------------------------------------------------------------------

/// Lifecycle status of a stored [`UsageRecord`]. Defaults to `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UsageRecordStatus {
    /// Live, counts toward aggregates, may be referenced by a compensation.
    #[default]
    Active,
    /// Removed from aggregates by an atomic depth-1 cascade; compensations
    /// referencing this row are rejected per the L1 `corrects_id` rule.
    Inactive,
}

/// Single usage record. The persisted shape is the canonical return value
/// of every create surface (new insert or silent idempotency replay).
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-usage-record:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-idempotency-key:p1
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    /// Caller-supplied record UUID.
    pub uuid: Uuid,
    /// Usage type this record attaches to.
    pub gts_id: UsageTypeGtsId,
    /// Owning tenant for this record. Caller-supplied; PDP uses it as the
    /// `OWNER_TENANT_ID` attribute.
    pub tenant_id: Uuid,
    /// Resource attribution composite (mandatory).
    pub resource_ref: ResourceRef,
    /// Optional subject attribution composite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<SubjectRef>,
    /// Caller-supplied metadata. Keys are validated [`MetadataKey`]s and
    /// values are typed as `String` end-to-end; closed-shape membership
    /// against the usage type's `metadata_fields` and the operator-configured
    /// size cap are enforced at the gateway before plugin dispatch. Omitted
    /// from the wire when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<MetadataKey, String>,
    /// Signed numeric measurement value, carried as a fixed-precision
    /// [`rust_decimal::Decimal`] on every surface (SDK, REST, plugin SPI)
    /// and persisted as Postgres `NUMERIC`. The wire encoding is a JSON
    /// string (`"42.5"`) — never a JSON number — so client/server number
    /// representations cannot round-trip through float and silently lose
    /// precision. The permitted sign is jointly governed by the usage
    /// type's [`UsageKind`] and the presence of `corrects_id` per the
    /// four-cell value matrix.
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
    /// Mandatory caller-supplied key for at-least-once-with-dedup semantics.
    /// The plugin SPI dedups on `(tenant_id, usage_type_gts_id, idempotency_key)`.
    pub idempotency_key: IdempotencyKey,
    /// When set, marks this row as a counter compensation referencing a
    /// previously emitted ordinary usage row. The four-cell value matrix
    /// and the L1 referential rule are enforced at the gateway before
    /// plugin dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects_id: Option<Uuid>,
    /// Record lifecycle status.
    #[serde(default)]
    pub status: UsageRecordStatus,
    /// Record creation timestamp (RFC 3339 on the wire).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Time-window input shared by `list_usage_records` and
// `query_aggregated_usage_records`
// ---------------------------------------------------------------------------

/// Half-open `[from, to)` time window in UTC.
///
/// Constructor [`Self::new`] enforces `from < to`; `Deserialize` routes
/// through `new` so wire payloads cannot bypass the invariant. Both bounds
/// serialize as RFC-3339 UTC strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeWindow {
    #[serde(with = "time::serde::rfc3339")]
    from: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    to: time::OffsetDateTime,
}

impl TimeWindow {
    /// Creates a [`TimeWindow`] after validating that `from < to`.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidTimeRange`] when `from >= to`.
    pub fn new(
        from: time::OffsetDateTime,
        to: time::OffsetDateTime,
    ) -> Result<Self, UsageCollectorError> {
        if from >= to {
            return Err(UsageCollectorError::InvalidTimeRange { from, to });
        }
        Ok(Self { from, to })
    }

    /// Inclusive lower bound.
    #[must_use]
    pub fn from(&self) -> time::OffsetDateTime {
        self.from
    }

    /// Exclusive upper bound.
    #[must_use]
    pub fn to(&self) -> time::OffsetDateTime {
        self.to
    }
}

impl<'de> Deserialize<'de> for TimeWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(with = "time::serde::rfc3339")]
            from: time::OffsetDateTime,
            #[serde(with = "time::serde::rfc3339")]
            to: time::OffsetDateTime,
        }
        let Raw { from, to } = Raw::deserialize(deserializer)?;
        TimeWindow::new(from, to).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Aggregated-query surface
// ---------------------------------------------------------------------------

/// Aggregation function applied to the filtered `UsageRecord.value` stream.
///
/// `Count` counts matched rows and is well-defined for any value shape. The
/// other variants require a numeric `value`; non-numeric values surface as a
/// validation error from the plugin at execution time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AggregationOp {
    /// Sum of matched values. Compensation rows contribute their signed value.
    Sum,
    /// Count of matched rows.
    Count,
    /// Minimum matched value.
    Min,
    /// Maximum matched value.
    Max,
    /// Mean of matched values.
    Avg,
}

/// Dimension to group an aggregation by.
///
/// Each variant is a column or JSON-key facet of the underlying record
/// stream. `Metadata(String)` carries a single declared metadata key
/// (validated against the queried usage type's `metadata_fields`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationDimension {
    /// Group by owning tenant.
    TenantId,
    /// Group by `resource_ref.resource_id`.
    ResourceId,
    /// Group by `resource_ref.resource_type`.
    ResourceType,
    /// Group by `subject_ref.subject_id` (rows without a subject are
    /// excluded from the grouping).
    SubjectId,
    /// Group by `subject_ref.subject_type` (rows without a `subject_type`
    /// are excluded from the grouping).
    SubjectType,
    /// Group by the value of a single declared metadata key.
    Metadata(MetadataKey),
}

/// Aggregation specification: what to compute and how to slice it.
///
/// `op` is the aggregation function; `group_by` is the ordered list of
/// dimensions. An empty `group_by` yields a single result bucket with an
/// empty key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationSpec {
    pub op: AggregationOp,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<AggregationDimension>,
}

/// Single aggregated bucket. One entry per element in
/// [`AggregationSpec::group_by`], in the same order; empty when `group_by`
/// was empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregationBucket {
    /// Dimension key values in [`AggregationSpec::group_by`] order. Each
    /// entry is the string form of the corresponding [`AggregationDimension`]:
    ///
    /// - [`AggregationDimension::TenantId`] — `Uuid::to_string()`, the
    ///   canonical lowercase hyphenated form
    ///   (`01234567-89ab-cdef-0123-456789abcdef`).
    /// - [`AggregationDimension::ResourceId`],
    ///   [`AggregationDimension::ResourceType`],
    ///   [`AggregationDimension::SubjectId`],
    ///   [`AggregationDimension::SubjectType`] — the record's corresponding
    ///   identifier or type string verbatim.
    /// - [`AggregationDimension::Metadata`] — the metadata value at the
    ///   declared key, which is already a `String` (or string-coercible)
    ///   per the [`UsageType::metadata_fields`] closed-shape rule.
    ///
    /// Plugins own this string-form contract at bucket-construction time;
    /// the SDK does not transform values at the boundary. Empty when
    /// `group_by` was empty (the no-grouping case yields a single bucket).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<String>,
    /// Aggregation result for the bucket, carried as a fixed-precision
    /// [`Decimal`] so `SUM`, `MIN`, `MAX`, and `COUNT` are exact and
    /// compensation rows net to zero. `AVG` may carry a plugin-chosen
    /// rounding scale on non-terminating quotients. `None` when no rows
    /// matched the bucket (e.g. `MIN` over an empty set). Wire-encoded as
    /// a JSON string for the same float-round-trip reason as
    /// [`UsageRecord::value`].
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub value: Option<Decimal>,
}

/// Aggregated-query result.
///
/// A single bucket with an empty `key` represents the no-grouping case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregationResult {
    /// Result buckets in plugin-emitted order.
    pub buckets: Vec<AggregationBucket>,
}

// ---------------------------------------------------------------------------
// Filter surface for `list_usage_records`
// ---------------------------------------------------------------------------
//
// `UsageRecordQuery` declares the filterable-field schema for the OData
// surface of `list_usage_records`. The struct is never constructed at
// runtime; it exists solely to feed `#[derive(ODataFilterable)]`, which
// generates [`UsageRecordQueryFilterField`] and its
// [`toolkit_odata::filter::FilterField`] impl. Plugin implementations supply
// a `FieldToColumn<UsageRecordFilterField>` mapper next to their entity
// definition; the SDK does not encode storage-layer column mapping.
//
// `gts_id` is intentionally absent from this schema. It is carried as a
// typed parameter on `list_usage_records` /
// `query_aggregated_usage_records`; omitting it here means
// `parse_odata_filter::<UsageRecordFilterField>` rejects any
// `gts_id`-touching predicate at parse time as
// `FilterError::UnknownField`, so neither plugins nor the gateway need a
// runtime reject path.
//
// Nested attribution composites (`resource_ref`, `subject_ref`) are
// flattened to their leaf identifiers (`resource_id`, `resource_type`,
// `subject_id`, `subject_type`) so filtering goes through the macro-derived
// path rather than a hand-rolled slash-path `FilterField` impl.
//
// `status` is declared `String` on the filter wire (`"active"` /
// `"inactive"`); plugins translate to their storage representation via
// `FieldToColumn::map_value`.
//
// `metadata` filtering does not flow through OData — see
// [`MetadataFilter`] below, supplied as a separate parameter on
// `list_usage_records`. Postgres has no general `serde_json::Value` filter
// surface in `toolkit-odata`, and there is no precedent for one in the
// workspace.

/// Filterable-field schema for `list_usage_records`'s `ODataQuery` argument.
///
/// Never constructed at runtime. The `dead_code` allow is intentional —
/// the struct is a derive-only artifact (see file-level comment above for
/// rationale).
#[derive(ODataFilterable)]
#[allow(dead_code)]
pub struct UsageRecordQuery {
    /// `usage_records.tenant_id` (owning tenant). Supports `eq` and `in`.
    #[odata(filter(kind = "Uuid"))]
    pub tenant_id: Uuid,
    /// `usage_records.resource_ref.resource_id`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub resource_id: String,
    /// `usage_records.resource_ref.resource_type`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub resource_type: String,
    /// `usage_records.subject_ref.subject_id`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub subject_id: String,
    /// `usage_records.subject_ref.subject_type`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub subject_type: String,
    /// `usage_records.corrects_id` (compensation target). Supports `eq`
    /// and `in`.
    #[odata(filter(kind = "Uuid"))]
    pub corrects_id: Uuid,
    /// `usage_records.status` lifecycle (`"active"` / `"inactive"`). Plugins
    /// translate to the storage representation via
    /// `FieldToColumn::map_value`.
    #[odata(filter(kind = "String"))]
    pub status: String,
}

pub use UsageRecordQueryFilterField as UsageRecordFilterField;

/// Equality-set filter applied to a single [`UsageRecord::metadata`] key.
///
/// `metadata` is a `serde_json::Value` map whose keys are not part of any
/// static schema; the `OData` filter surface in `toolkit-odata` cannot
/// express filtering on dynamic JSON keys. `MetadataFilter` is the typed
/// side-channel used by [`crate::UsageCollectorClientV1::list_usage_records`]
/// and the plugin SPI to filter on those keys.
///
/// Semantics across a `&[MetadataFilter]`:
///
/// - AND across distinct filters (different keys).
/// - OR within a single filter's `values()`.
/// - An empty slice imposes no metadata filter.
///
/// Constructor [`Self::new`] enforces a validated [`MetadataKey`] (non-empty,
/// no NUL bytes) and a non-empty value set; `Deserialize` routes through
/// `new` so wire payloads cannot bypass validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataFilter {
    key: MetadataKey,
    values: Vec<String>,
}

impl MetadataFilter {
    /// Creates a [`MetadataFilter`] after validating `key` (via
    /// [`MetadataKey::new`]) and that `values` carries at least one entry.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidMetadataFilter`] when the key
    /// is empty or contains a NUL byte, or when `values` is empty. A
    /// failing `MetadataKey::new` is rewrapped as
    /// `InvalidMetadataFilter` so callers see one variant for the whole
    /// `MetadataFilter::new` boundary.
    pub fn new(
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, UsageCollectorError> {
        let key = MetadataKey::new(key).map_err(|err| match err {
            UsageCollectorError::InvalidMetadataKey { reason } => {
                UsageCollectorError::InvalidMetadataFilter { reason }
            }
            other => other,
        })?;
        let values: Vec<String> = values.into_iter().map(Into::into).collect();
        if values.is_empty() {
            return Err(UsageCollectorError::InvalidMetadataFilter {
                reason: format!("metadata filter for key `{key}` must carry at least one value"),
            });
        }
        Ok(Self { key, values })
    }

    /// Borrows the metadata key.
    #[must_use]
    pub fn key(&self) -> &MetadataKey {
        &self.key
    }

    /// Borrows the candidate value set.
    #[must_use]
    pub fn values(&self) -> &[String] {
        &self.values
    }
}

impl<'de> Deserialize<'de> for MetadataFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            key: String,
            values: Vec<String>,
        }
        let Raw { key, values } = Raw::deserialize(deserializer)?;
        MetadataFilter::new(key, values).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
