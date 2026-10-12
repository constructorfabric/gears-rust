//! `ClickHouse` row structs mirroring `usage_records` and `usage_type_catalog`
//! (see `migrations/0001_init.sql`).
//!
//! Column types match the DDL:
//! - `UUID` → [`uuid::Uuid`] (see [`ch_uuid`])
//! - `DateTime64(6)` → `i64` epoch-microseconds ([`EpochMicros`])
//! - `Decimal128(9)` → [`rust_decimal::Decimal`] (see [`ch_decimal128_9`])
//! - `Map(String, String)` → [`HashMap`]`<String, String>` ([`RawMetadata`] / [`ValidatedMetadata`])
//! - `Enum8(…)` → [`UsageRecordStatusCode`] / [`UsageTypeKindCode`]
//! - `Array(String)` → `Vec<String>`
//! - `Nullable(T)` → `Option<T>`
//! - `UInt64` → `u64`

use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;

use rust_decimal::Decimal;
use serde_repr::{Deserialize_repr, Serialize_repr};
use time::OffsetDateTime;
use usage_collector_sdk::{
    IdempotencyKey, MetadataKey, ResourceRef, SubjectRef, UsageCollectorPluginError, UsageKind,
    UsageRecord, UsageRecordStatus, UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

/// `RowBinary`-compatible serde helpers for `UUID` columns.
///
/// Apply with `#[serde(with = "ch_uuid")]`.
pub(crate) mod ch_uuid {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use uuid::Uuid;

    pub fn serialize<S: Serializer>(u: &Uuid, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            u.to_string().serialize(s)
        } else {
            u.as_u64_pair().serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
        if d.is_human_readable() {
            let s: String = Deserialize::deserialize(d)?;
            Uuid::parse_str(&s).map_err(serde::de::Error::custom)
        } else {
            let (hi, lo): (u64, u64) = Deserialize::deserialize(d)?;
            Ok(Uuid::from_u64_pair(hi, lo))
        }
    }
}

/// `RowBinary`-compatible serde helper for `Nullable(UUID)` columns.
///
/// Apply with `#[serde(with = "ch_uuid_opt")]`.
pub(crate) mod ch_uuid_opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use uuid::Uuid;

    #[allow(
        clippy::ref_option,
        reason = "required signature for serde with-module"
    )]
    pub fn serialize<S: Serializer>(opt: &Option<Uuid>, s: S) -> Result<S::Ok, S::Error> {
        match opt {
            Some(u) => {
                if s.is_human_readable() {
                    Some(u.to_string()).serialize(s)
                } else {
                    Some(u.as_u64_pair()).serialize(s)
                }
            }
            None => {
                if s.is_human_readable() {
                    None::<String>.serialize(s)
                } else {
                    None::<(u64, u64)>.serialize(s)
                }
            }
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
        if d.is_human_readable() {
            let opt: Option<String> = Deserialize::deserialize(d)?;
            opt.map(|s| Uuid::parse_str(&s).map_err(serde::de::Error::custom))
                .transpose()
        } else {
            let opt: Option<(u64, u64)> = Deserialize::deserialize(d)?;
            Ok(opt.map(|(hi, lo)| Uuid::from_u64_pair(hi, lo)))
        }
    }
}

/// `RowBinary`-compatible serde helper for `Decimal128(9)` columns.
///
/// Apply with `#[serde(with = "ch_decimal128_9")]`. The binary encoding is
/// fail-closed: a value that is not exactly representable at scale 9 is
/// refused rather than rounded (see [`ch_decimal128_9::scaled_mantissa`]).
pub(crate) mod ch_decimal128_9 {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Scale of the `usage_records.value` column (`Decimal128(9)`).
    ///
    /// **Accepted decision.** The gear contract publishes quantities with up
    /// to 28 significant digits and up to 28 fractional digits
    /// (`docs/usage-collector-v1.yaml`, `UsageQuantity`), which no
    /// `Decimal128` scale can hold losslessly: 28 fractional plus 28 integer
    /// digits needs `Decimal256(28)`, and with it 256-bit mantissa arithmetic
    /// on both the write and read paths. Nine fractional digits are accepted
    /// as sufficient for metering, and the narrower column is kept. The
    /// consequence is enforced rather than hidden: a value with non-zero
    /// digits beyond the ninth fractional place is refused by
    /// [`scaled_mantissa`] before it is written, never rounded. Recorded in
    /// the plugin PRD §12 and DESIGN §3.7.
    const SCALE: u32 = 9;

    /// Convert `d` to the scale-9 mantissa `ClickHouse` stores for a
    /// `Decimal128(9)` column, refusing any value the column cannot hold
    /// exactly.
    ///
    /// Trailing zeros beyond scale 9 (e.g. `1.0000000000`) are exactly
    /// representable and are accepted; the result is the same mantissa as
    /// for `1.000000000`.
    ///
    /// # Errors
    ///
    /// - The value has non-zero digits beyond the ninth fractional place.
    ///   `rust_decimal::Decimal::rescale` would silently round them away,
    ///   which would persist a usage amount other than the one supplied.
    /// - The mantissa cannot be multiplied up to scale 9 without exceeding
    ///   `rust_decimal`'s 96-bit capacity, so `rescale` stops short and the
    ///   resulting mantissa would encode the wrong `Decimal128(9)`.
    pub fn scaled_mantissa(d: &Decimal) -> Result<i128, String> {
        let mut rescaled = *d;
        // `rescale` never fails: lowering the scale rounds, and raising it
        // stops short when the mantissa would overflow. Both outcomes are
        // detected below instead of being encoded.
        rescaled.rescale(SCALE);
        if rescaled.scale() != SCALE {
            return Err(format!(
                "Decimal128(9) value cannot be represented at scale {SCALE} (got scale {})",
                rescaled.scale()
            ));
        }
        // `Decimal` equality is numeric across scales, so any difference here
        // means non-zero fractional digits were rounded away.
        if rescaled != *d {
            return Err(format!(
                "Decimal128(9) value {d} has {} fractional digits and cannot be stored at scale {SCALE} without rounding",
                d.scale()
            ));
        }
        Ok(rescaled.mantissa())
    }

    pub fn serialize<S: Serializer>(d: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            d.to_string().serialize(s)
        } else {
            scaled_mantissa(d)
                .map_err(serde::ser::Error::custom)?
                .serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        if d.is_human_readable() {
            let s: String = Deserialize::deserialize(d)?;
            s.parse::<Decimal>().map_err(serde::de::Error::custom)
        } else {
            let raw: i128 = Deserialize::deserialize(d)?;
            let negative = raw < 0;
            let abs_val = raw.unsigned_abs();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "abs_val >> 96 guard ensures lower 96 bits are the only non-zero bits"
            )]
            let lo = abs_val as u32;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "abs_val >> 96 guard ensures lower 96 bits are the only non-zero bits"
            )]
            let mid = (abs_val >> 32) as u32;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "abs_val >> 96 guard ensures lower 96 bits are the only non-zero bits"
            )]
            let hi = (abs_val >> 64) as u32;
            if abs_val >> 96 != 0 {
                return Err(serde::de::Error::custom(
                    "Decimal128(9) value overflows `rust_decimal::Decimal`",
                ));
            }
            Ok(Decimal::from_parts(lo, mid, hi, negative, SCALE))
        }
    }
}

/// Epoch-microseconds form of a `ClickHouse` `DateTime64(6)` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochMicros(pub i64);

impl From<OffsetDateTime> for EpochMicros {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "practical timestamps fit in i64"
    )]
    fn from(dt: OffsetDateTime) -> Self {
        Self(usage_collector_sdk::created_at_micros(dt) as i64)
    }
}

impl TryFrom<EpochMicros> for OffsetDateTime {
    type Error = UsageCollectorPluginError;

    fn try_from(EpochMicros(micros): EpochMicros) -> Result<Self, Self::Error> {
        Self::from_unix_timestamp_nanos(i128::from(micros) * 1_000).map_err(|e| {
            UsageCollectorPluginError::internal(format!(
                "stored timestamp {micros} µs out of range: {e}"
            ))
        })
    }
}

/// `ClickHouse` `Map(String, String)` awaiting [`MetadataKey`] validation.
#[derive(Debug, Clone, Default)]
pub struct RawMetadata(pub HashMap<String, String>);

impl From<&BTreeMap<MetadataKey, String>> for RawMetadata {
    fn from(map: &BTreeMap<MetadataKey, String>) -> Self {
        Self(
            map.iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.clone()))
                .collect(),
        )
    }
}

impl<S: BuildHasher + Default> From<RawMetadata> for HashMap<String, String, S> {
    fn from(RawMetadata(map): RawMetadata) -> Self {
        map.into_iter().collect()
    }
}

/// Validated metadata map rebuilt from a stored `Map(String, String)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidatedMetadata(pub BTreeMap<MetadataKey, String>);

impl TryFrom<RawMetadata> for ValidatedMetadata {
    type Error = UsageCollectorPluginError;

    fn try_from(RawMetadata(map): RawMetadata) -> Result<Self, Self::Error> {
        let mut out = BTreeMap::new();
        for (key, val) in map {
            let metadata_key = MetadataKey::new(key).map_err(|e| {
                UsageCollectorPluginError::internal(format!("stored metadata key invalid: {e}"))
            })?;
            out.insert(metadata_key, val);
        }
        Ok(Self(out))
    }
}

impl From<ValidatedMetadata> for BTreeMap<MetadataKey, String> {
    fn from(ValidatedMetadata(map): ValidatedMetadata) -> Self {
        map
    }
}

impl TryFrom<HashMap<String, String>> for ValidatedMetadata {
    type Error = UsageCollectorPluginError;

    fn try_from(map: HashMap<String, String>) -> Result<Self, Self::Error> {
        Self::try_from(RawMetadata(map))
    }
}

/// `Enum8('active' = 1, 'inactive' = 2)` — discriminants match `migrations/0001_init.sql`.
///
/// Storage encoding only: the wire form is the lowercase string of
/// [`UsageRecordStatus`]. Discriminants are permanent — never renumber or
/// reuse one; a new variant takes the next unused value alongside a migration
/// extending the `Enum8` (see DESIGN.md §3.7 "`Enum8` value stability").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i8)]
pub enum UsageRecordStatusCode {
    Active = 1,
    Inactive = 2,
}

impl From<UsageRecordStatusCode> for UsageRecordStatus {
    fn from(code: UsageRecordStatusCode) -> Self {
        match code {
            UsageRecordStatusCode::Active => Self::Active,
            UsageRecordStatusCode::Inactive => Self::Inactive,
        }
    }
}

impl From<UsageRecordStatus> for UsageRecordStatusCode {
    fn from(status: UsageRecordStatus) -> Self {
        match status {
            UsageRecordStatus::Active => Self::Active,
            UsageRecordStatus::Inactive => Self::Inactive,
        }
    }
}

impl From<UsageRecordStatusCode> for &'static str {
    fn from(code: UsageRecordStatusCode) -> Self {
        match code {
            UsageRecordStatusCode::Active => "active",
            UsageRecordStatusCode::Inactive => "inactive",
        }
    }
}

/// One row of the `usage_records` table.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize, serde::Serialize)]
pub struct UsageRecordRow {
    #[serde(with = "ch_uuid")]
    pub id: Uuid,
    #[serde(with = "ch_uuid")]
    pub tenant_id: Uuid,
    pub gts_id: String,
    #[serde(with = "ch_decimal128_9")]
    pub value: Decimal,
    pub created_at: i64,
    pub resource_id: String,
    pub resource_type: String,
    pub subject_id: Option<String>,
    pub subject_type: Option<String>,
    pub idempotency_key: String,
    #[serde(with = "ch_uuid_opt")]
    pub corrects_id: Option<Uuid>,
    pub status: UsageRecordStatusCode,
    pub metadata: HashMap<String, String>,
    pub ingested_at: i64,
    pub version: u64,
}

fn stored_gts_id(raw: &str) -> Result<UsageTypeGtsId, UsageCollectorPluginError> {
    UsageTypeGtsId::new(raw).map_err(|e| {
        UsageCollectorPluginError::internal(format!("stored gts_id `{raw}` invalid: {e}"))
    })
}

impl TryFrom<UsageRecordRow> for UsageRecord {
    type Error = UsageCollectorPluginError;

    fn try_from(row: UsageRecordRow) -> Result<Self, Self::Error> {
        let gts_id = stored_gts_id(&row.gts_id)?;

        let resource_ref = ResourceRef::new(row.resource_id, row.resource_type).map_err(|e| {
            UsageCollectorPluginError::internal(format!("stored resource_ref invalid: {e}"))
        })?;

        let subject_ref = match row.subject_id {
            Some(subject_id) => {
                Some(SubjectRef::new(subject_id, row.subject_type).map_err(|e| {
                    UsageCollectorPluginError::internal(format!("stored subject_ref invalid: {e}"))
                })?)
            }
            None => None,
        };

        let idempotency_key = IdempotencyKey::new(row.idempotency_key).map_err(|e| {
            UsageCollectorPluginError::internal(format!("stored idempotency_key invalid: {e}"))
        })?;

        let metadata = ValidatedMetadata::try_from(row.metadata)?.0;
        let status = UsageRecordStatus::from(row.status);
        let created_at = OffsetDateTime::try_from(EpochMicros(row.created_at))?;

        Ok(Self {
            id: row.id,
            gts_id,
            tenant_id: row.tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            value: row.value,
            idempotency_key,
            corrects_id: row.corrects_id,
            status,
            created_at,
        })
    }
}

impl From<(&UsageRecord, u64)> for UsageRecordRow {
    fn from((record, version): (&UsageRecord, u64)) -> Self {
        let now_micros = EpochMicros::from(OffsetDateTime::now_utc()).0;
        Self {
            id: record.id,
            tenant_id: record.tenant_id,
            gts_id: record.gts_id.as_ref().to_owned(),
            value: record.value,
            created_at: EpochMicros::from(record.created_at).0,
            resource_id: record.resource_ref.resource_id().to_owned(),
            resource_type: record.resource_ref.resource_type().to_owned(),
            subject_id: record
                .subject_ref
                .as_ref()
                .map(|s| s.subject_id().to_owned()),
            subject_type: record
                .subject_ref
                .as_ref()
                .and_then(|s| s.subject_type())
                .map(str::to_owned),
            idempotency_key: record.idempotency_key.as_str().to_owned(),
            corrects_id: record.corrects_id,
            status: UsageRecordStatus::Active.into(),
            metadata: RawMetadata::from(&record.metadata).into(),
            ingested_at: now_micros,
            version,
        }
    }
}

/// `Enum8('counter' = 1, 'gauge' = 2)` — discriminants match `migrations/0001_init.sql`.
///
/// Storage encoding only: the wire form is the lowercase string of
/// [`UsageKind`]. Discriminants are permanent — never renumber or reuse one;
/// a new variant takes the next unused value alongside a migration extending
/// the `Enum8` (see DESIGN.md §3.7 "`Enum8` value stability").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i8)]
pub enum UsageTypeKindCode {
    Counter = 1,
    Gauge = 2,
}

impl From<UsageTypeKindCode> for UsageKind {
    fn from(code: UsageTypeKindCode) -> Self {
        match code {
            UsageTypeKindCode::Counter => Self::Counter,
            UsageTypeKindCode::Gauge => Self::Gauge,
        }
    }
}

impl From<UsageKind> for UsageTypeKindCode {
    fn from(kind: UsageKind) -> Self {
        match kind {
            UsageKind::Counter => Self::Counter,
            UsageKind::Gauge => Self::Gauge,
        }
    }
}

/// One row of the `usage_type_catalog` table.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize, serde::Serialize)]
pub struct UsageTypeRow {
    pub gts_id: String,
    pub kind: UsageTypeKindCode,
    pub metadata_fields: Vec<String>,
    pub version: u64,
}

impl TryFrom<UsageTypeRow> for UsageType {
    type Error = UsageCollectorPluginError;

    fn try_from(row: UsageTypeRow) -> Result<Self, Self::Error> {
        let gts_id = stored_gts_id(&row.gts_id)?;
        let kind = row.kind.into();

        let mut metadata_fields = std::collections::BTreeSet::new();
        for field in row.metadata_fields {
            let key = MetadataKey::new(field).map_err(|e| {
                UsageCollectorPluginError::internal(format!(
                    "stored metadata_fields entry invalid: {e}"
                ))
            })?;
            metadata_fields.insert(key);
        }

        Ok(Self {
            gts_id,
            kind,
            metadata_fields,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "entity_tests.rs"]
mod entity_tests;
