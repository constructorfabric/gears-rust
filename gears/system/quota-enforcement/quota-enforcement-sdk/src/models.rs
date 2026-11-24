//! Domain types used by quota-enforcement contracts.
//!
//! - Every enum is closed at the SDK boundary. Deserialization rejects an
//!   unknown value instead of a fallback variant.
//! - GTS-anchored enums (`QuotaType`, `EnforcementMode`, `QuotaSource`,
//!   `PeriodType`) serialize as their full GTS instance id. Storage rows and
//!   events carry that form.
//! - Timestamps serialize as RFC 3339 in UTC.
//! - Input shapes reject unknown fields.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use gts::{GtsIdError, GtsTypeId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::serde::rfc3339;
use toolkit_gts::gts_id;
use toolkit_security::crypto::hash::Sha256;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            utoipa::ToSchema,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wrap an existing identifier.
            #[must_use]
            pub const fn new(id: Uuid) -> Self {
                Self(id)
            }

            /// Mint a fresh, time-ordered identifier (`UUIDv7`).
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// The raw UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id!(
    /// Server-assigned Quota identifier. `UUIDv7`, so that lexicographic order is
    /// acquisition order (ADR-0002).
    QuotaId
);
uuid_id!(
    /// Tenant identifier. PDP-authorized before it reaches storage.
    TenantId
);
uuid_id!(
    /// Opaque two-phase lease token.
    LeaseToken
);
uuid_id!(
    /// Identifier of one consumption-period counter row.
    PeriodId
);
uuid_id!(
    /// Notification outbox event identifier.
    EventId
);

/// Stable identifier of a Quota Resolution Policy. The seeded platform policy
/// is [`PolicyId::GLOBAL`].
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct PolicyId(String);

impl PolicyId {
    /// Identifier of the seeded `global` policy. It cannot be deleted.
    pub const GLOBAL: &'static str = "global";

    /// Wrap an identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The seeded global policy identifier.
    #[must_use]
    pub fn global() -> Self {
        Self(Self::GLOBAL.to_owned())
    }

    /// True for the seeded global policy.
    #[must_use]
    pub fn is_global(&self) -> bool {
        self.0 == Self::GLOBAL
    }

    /// The raw identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PolicyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A metric type id: a GTS type derived from `gts.cf.core.quota.metric.v1~`,
/// registered by the module that owns the metric (ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(transparent)]
#[schema(value_type = String)]
pub struct MetricId(GtsTypeId);

impl MetricId {
    /// Wrap an already validated type id.
    #[must_use]
    pub const fn new(id: GtsTypeId) -> Self {
        Self(id)
    }

    /// Parse a full GTS type id.
    ///
    /// # Errors
    ///
    /// Returns the GTS parse error when `raw` is not a well-formed type id.
    pub fn parse(raw: &str) -> Result<Self, GtsIdError> {
        GtsTypeId::try_new(raw).map(Self)
    }

    /// The underlying type id.
    #[must_use]
    pub const fn as_gts(&self) -> &GtsTypeId {
        &self.0
    }

    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl fmt::Display for MetricId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

macro_rules! digest_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wrap a SHA-256 digest.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The raw digest bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Lowercase hexadecimal form.
            #[must_use]
            pub fn to_hex(self) -> String {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut out = String::with_capacity(64);
                for byte in self.0 {
                    out.push(char::from(HEX[usize::from(byte >> 4)]));
                    out.push(char::from(HEX[usize::from(byte & 0x0f)]));
                }
                out
            }

            /// Parse the lowercase or uppercase hexadecimal form.
            ///
            /// # Errors
            ///
            /// Returns [`UnknownValue`] when `hex` is not 64 hexadecimal digits.
            pub fn parse_hex(hex: &str) -> Result<Self, UnknownValue> {
                let bad = || UnknownValue {
                    kind: stringify!($name),
                    value: hex.to_owned(),
                };
                // `from_str_radix` alone would accept a `+` sign in a pair.
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(bad());
                }
                let mut bytes = [0_u8; 32];
                for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
                    let pair = std::str::from_utf8(chunk).map_err(|_| bad())?;
                    bytes[i] = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
                }
                Ok(Self(bytes))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw: Cow<'de, str> = Deserialize::deserialize(d)?;
                Self::parse_hex(&raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

digest_newtype!(
    /// SHA-256 of the canonical sorted-JSON request payload.
    PayloadHash
);
digest_newtype!(
    /// SHA-256 of the attribution admission accepted for a charged operation:
    /// the tenant, the subjects, and the metric with its type and optional id
    /// (ADR-0011). A charge keeps it in place of any subject identifier.
    AttributionDigest
);

/// Returns JSON with object keys sorted at every depth, independent of
/// `serde_json`'s `preserve_order` feature.
fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, nested)| (key.clone(), canonical_json(nested)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// SHA-256 over the canonical JSON form of `payload`.
fn hash_canonical<T: Serialize>(payload: &T) -> Result<[u8; 32], serde_json::Error> {
    let value = canonical_json(&serde_json::to_value(payload)?);
    let bytes = serde_json::to_vec(&value)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(finish(hasher))
}

/// The 32 digest bytes, as an array rather than the slice `finish` returns.
fn finish(hasher: Sha256) -> [u8; 32] {
    let digest = hasher.finalize();
    let mut bytes = [0; 32];
    bytes.copy_from_slice(digest.as_ref());
    bytes
}

impl PayloadHash {
    /// Digest of a request payload in its canonical form: key order and the
    /// `preserve_order` feature do not change the result, array order does
    /// (PRD section 5.8).
    ///
    /// # Errors
    ///
    /// Returns the serialization error when `payload` cannot become JSON, for
    /// example a map with non-string keys or a type whose `Serialize` fails.
    pub fn of_canonical<T: Serialize>(payload: &T) -> Result<Self, serde_json::Error> {
        hash_canonical(payload).map(Self::from_bytes)
    }
}

impl AttributionDigest {
    /// The digest in its fixed form: the tenant id; the subjects other than
    /// the tenant subject, which the tenant id already covers, ordered by
    /// subject type id; the metric type id; the metric id or its absence. The
    /// metric `metadata` is not part of it, so a rollback that names the same
    /// attribution always gets the same digest.
    #[must_use]
    pub fn of(
        tenant_id: TenantId,
        subjects: &[SubjectRef],
        metric: &MetricId,
        metric_id: Option<&str>,
    ) -> Self {
        // Length prefixes keep field boundaries unambiguous.
        fn field(hasher: &mut Sha256, value: &str) {
            let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
            hasher.update(len.to_be_bytes());
            hasher.update(value.as_bytes());
        }
        let mut named: Vec<(&str, &str)> = subjects
            .iter()
            .filter(|subject| !subject.is_tenant())
            .map(|subject| (subject.subject_type.as_ref(), subject.subject_id.as_str()))
            .collect();
        named.sort_unstable();
        let mut hasher = Sha256::new();
        hasher.update(tenant_id.as_uuid().as_bytes());
        hasher.update(u32::try_from(named.len()).unwrap_or(u32::MAX).to_be_bytes());
        for (subject_type, subject_id) in named {
            field(&mut hasher, subject_type);
            field(&mut hasher, subject_id);
        }
        field(&mut hasher, metric.as_str());
        match metric_id {
            Some(id) => {
                hasher.update([1]);
                field(&mut hasher, id);
            }
            None => hasher.update([0]),
        }
        Self::from_bytes(finish(hasher))
    }
}

/// An unknown value was supplied for a closed enum or a fixed-width digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value: {value}")]
pub struct UnknownValue {
    /// Which closed set rejected the value.
    pub kind: &'static str,
    /// The rejected value.
    pub value: String,
}

// ---------------------------------------------------------------------------
// GTS-anchored closed enums
// ---------------------------------------------------------------------------

macro_rules! gts_closed_enum {
    (
        $(#[$meta:meta])*
        $name:ident, kind = $kind:literal, base = $base:expr, {
            $( $(#[$vmeta:meta])* $variant:ident => $id:expr ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Abstract GTS base type every value derives from.
            pub const BASE_TYPE_ID: &'static str = $base;

            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// Full GTS instance id of this value: the wire and storage form.
            #[must_use]
            pub const fn as_gts_id(self) -> &'static str {
                match self {
                    $( Self::$variant => $id, )+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_gts_id())
            }
        }

        impl FromStr for $name {
            type Err = UnknownValue;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|value| value.as_gts_id() == s)
                    .ok_or_else(|| UnknownValue { kind: $kind, value: s.to_owned() })
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_gts_id())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw: Cow<'de, str> = Deserialize::deserialize(d)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }

        // The wire form is the GTS id string, one of `ALL`.
        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::String)
                    .enum_values(Some(Self::ALL.iter().map(|value| value.as_gts_id())))
                    .into()
            }
        }

        impl utoipa::ToSchema for $name {}
    };
}

gts_closed_enum!(
    /// Accounting model of a Quota (PRD section 5.3).
    QuotaType, kind = "quota_type", base = gts_id!("cf.core.qe.quota_type.v1~"), {
        /// In-flight reservable capacity, no period reset.
        Allocation => gts_id!("cf.core.qe.quota_type.v1~cf.core.qe.allocation.v1"),
        /// Per-period cumulative consumption, reset at the period boundary.
        Consumption => gts_id!("cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1"),
        /// Reserved for P3. Creation is rejected in P1.
        Rate => gts_id!("cf.core.qe.quota_type.v1~cf.core.qe.rate.v1"),
    }
);

gts_closed_enum!(
    /// Behaviour of a Quota at its cap boundary (PRD section 5.11).
    EnforcementMode, kind = "enforcement_mode", base = gts_id!("cf.core.qe.enforcement_type.v1~"), {
        /// Operations that would cross the cap are denied. The only P1 mode.
        Hard => gts_id!("cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1"),
    }
);

gts_closed_enum!(
    /// Who imposed the Quota (PRD section 5.2, "Source value semantics").
    QuotaSource, kind = "quota_source", base = gts_id!("cf.core.qe.source_type.v1~"), {
        /// Materialized from the licensing layer. The default.
        Licensing => gts_id!("cf.core.qe.source_type.v1~cf.core.qe.licensing.v1"),
        /// Created manually by an operator outside the licensing flow.
        Operator => gts_id!("cf.core.qe.source_type.v1~cf.core.qe.operator.v1"),
    }
);

gts_closed_enum!(
    /// Calendar-aligned UTC period of a consumption Quota (PRD section 5.4).
    PeriodType, kind = "period_type", base = gts_id!("cf.core.qe.period_type.v1~"), {
        /// 00:00 UTC to 24:00 UTC.
        Day => gts_id!("cf.core.qe.period_type.v1~cf.core.qe.day.v1"),
        /// Monday 00:00 UTC to the next Monday.
        Week => gts_id!("cf.core.qe.period_type.v1~cf.core.qe.week.v1"),
        /// First day of the month, 00:00 UTC.
        Month => gts_id!("cf.core.qe.period_type.v1~cf.core.qe.month.v1"),
        /// First of January, 00:00 UTC.
        Year => gts_id!("cf.core.qe.period_type.v1~cf.core.qe.year.v1"),
        /// Non-recurring. No automatic reset.
        OneTime => gts_id!("cf.core.qe.period_type.v1~cf.core.qe.one_time.v1"),
    }
);

// ---------------------------------------------------------------------------
// Plain closed enums
// ---------------------------------------------------------------------------

/// Lifecycle state of a Quota record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    /// Accepts debits and leases.
    Active,
    /// Retained for reads. Accepts no new debits or leases.
    Deactivated,
}

/// Lease state machine. Every state except `Active` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Capacity is held.
    Active,
    /// Converted into a debit.
    Committed,
    /// Held capacity returned by the holder.
    Released,
    /// TTL elapsed without commit or release.
    AutoReleased,
    /// Resolved atomically with the deactivation of a held Quota.
    ResolvedByDeactivation,
}

impl LeaseState {
    /// True when no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Active)
    }
}

/// State of one immutable Policy version (PRD section 5.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyVersionState {
    /// The version the latest pointer names. At most one per policy.
    Active,
    /// Replaced by a later active version.
    Superseded,
    /// Abandoned through rollback. Terminal.
    RolledBack,
    /// The previously active version of a soft-deleted policy. Terminal.
    Deleted,
}

/// Write operation kinds that carry an idempotency key (PRD section 5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    /// Single-shot debit.
    Debit,
    /// Compensating credit against one named Quota.
    Credit,
    /// Return of part of a charge (ADR-0011). Its key names the return
    /// record.
    Return,
    /// Lease acquisition.
    Reserve,
    /// Lease commit.
    Commit,
    /// Lease release.
    Release,
    /// Batch debit envelope.
    BatchDebit,
    /// Bulk Quota create envelope.
    BulkCreateQuotas,
    /// Bulk Quota update envelope.
    BulkUpdateQuotas,
    /// Bulk Quota deactivate envelope.
    BulkDeactivateQuotas,
}

impl OperationType {
    /// Every operation type, for decoding the storage discriminator.
    pub const ALL: [Self; 10] = [
        Self::Debit,
        Self::Credit,
        Self::Return,
        Self::Reserve,
        Self::Commit,
        Self::Release,
        Self::BatchDebit,
        Self::BulkCreateQuotas,
        Self::BulkUpdateQuotas,
        Self::BulkDeactivateQuotas,
    ];

    /// Stable `snake_case` name, also used as the storage discriminator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debit => "debit",
            Self::Credit => "credit",
            Self::Return => "return",
            Self::Reserve => "reserve",
            Self::Commit => "commit",
            Self::Release => "release",
            Self::BatchDebit => "batch_debit",
            Self::BulkCreateQuotas => "bulk_create_quotas",
            Self::BulkUpdateQuotas => "bulk_update_quotas",
            Self::BulkDeactivateQuotas => "bulk_deactivate_quotas",
        }
    }
}

/// Which kind of operation wrote the charge a rollback or a return names.
///
/// [`OperationType::Debit`] and [`OperationType::Commit`] are separate
/// idempotency namespaces, so one tenant may hold a debit and a lease commit
/// under the same key. The request names which of the two it means; there is
/// no fallback lookup that could reach the wrong one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RollbackableOperation {
    /// A direct debit, recorded under [`OperationType::Debit`]. The default, so
    /// a request written before leases existed keeps its meaning.
    #[default]
    Debit,
    /// A lease commit, recorded under [`OperationType::Commit`].
    LeaseCommit,
}

impl RollbackableOperation {
    /// The idempotency operation type the original was recorded under.
    #[must_use]
    pub const fn operation_type(self) -> OperationType {
        match self {
            Self::Debit => OperationType::Debit,
            Self::LeaseCommit => OperationType::Commit,
        }
    }
}

/// Closed notification event catalog (PRD section 5.15).
// @cpt-dod:cpt-cf-quota-enforcement-dod-event-catalog:p1
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationEventKind {
    /// Consumed amount crossed at least one configured threshold upward.
    ThresholdCrossed,
    /// A consumption Quota crossed a period boundary and settled.
    PeriodRollover,
    /// A lease TTL expired without commit or release.
    LeaseAutoReleased,
    /// A lease was resolved when its Quota was deactivated.
    LeaseResolvedByDeactivation,
    /// A Quota was created, updated, or deactivated.
    QuotaChanged,
    /// A credit was applied outside the debit and rollback flow.
    QuotaCounterAdjusted,
    /// A charge, in full or in part, was given back by a return or a
    /// rollback.
    QuotaRollbackApplied,
    /// A Policy was created, updated, rolled back, or deleted.
    PolicyChanged,
}

impl NotificationEventKind {
    /// Stable `kebab-case` name, equal to the serialized form; the outbox
    /// stores it as the payload type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThresholdCrossed => "threshold-crossed",
            Self::PeriodRollover => "period-rollover",
            Self::LeaseAutoReleased => "lease-auto-released",
            Self::LeaseResolvedByDeactivation => "lease-resolved-by-deactivation",
            Self::QuotaChanged => "quota-changed",
            Self::QuotaCounterAdjusted => "quota-counter-adjusted",
            Self::QuotaRollbackApplied => "quota-rollback-applied",
            Self::PolicyChanged => "policy-changed",
        }
    }
}

/// The `kind` trait of a metric type. Closed; the gear reads it from the
/// metric type's traits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Cumulative within a period; pairs naturally with consumption Quotas.
    Counter,
    /// A level; pairs naturally with allocation Quotas.
    Gauge,
}

// ---------------------------------------------------------------------------
// Subjects and metrics
// ---------------------------------------------------------------------------

/// A quota subject as a Quota stores it (ADR-0008): a quota subject type and
/// an opaque id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = QuotaSubjectRef)]
pub struct SubjectRef {
    /// A type derived from `gts.cf.core.quota.subj.v1~`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub subject_type: GtsTypeId,
    /// Opaque, non-empty subject identifier.
    #[serde(rename = "id")]
    pub subject_id: String,
}

impl SubjectRef {
    /// The tenant subject of `tenant_id`.
    #[must_use]
    pub fn tenant(tenant_id: TenantId) -> Self {
        Self {
            subject_type: GtsTypeId::new(quota_resolver_sdk::QUOTA_TENANT_SUBJECT_TYPE_ID),
            subject_id: tenant_id.to_string(),
        }
    }

    /// True for a subject of the tenant subject type.
    #[must_use]
    pub fn is_tenant(&self) -> bool {
        self.subject_type == quota_resolver_sdk::QUOTA_TENANT_SUBJECT_TYPE_ID
    }
}

/// One subject a request names, as text, so a malformed value is a canonical
/// `InvalidArgument` rather than a deserialization failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubjectClaim {
    /// A quota subject type id.
    #[serde(rename = "type")]
    pub subject_type: String,
    /// Opaque, non-empty subject identifier.
    pub id: String,
}

/// The metric object of a request (ADR-0013): the metric type, an optional id
/// of the one thing the amount is counted for, and the metadata policies read.
///
/// Optional fields distinguish omission from `null`; explicit `null` is
/// rejected and omitted values are not serialized. `metadata` is required on
/// the wire and never defaulted; it is an [`Option`] only so the service can
/// tell its omission apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricRef {
    /// The metric type id, as text.
    #[serde(rename = "type")]
    pub metric_type: String,
    /// Non-empty when present. Not part of the counter key.
    #[serde(
        default,
        deserialize_with = "present_or_error",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    /// Attributes of the operation, refined by the metric type's schema.
    #[serde(
        default,
        deserialize_with = "present_or_error",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Map<String, Value>>,
}

/// Caller-supplied attribution for an evaluation request. It remains untrusted
/// until admission authorizes the complete tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvaluationAttribution {
    /// Target tenant. QE adds its tenant subject to every request.
    pub tenant_id: TenantId,
    /// At least one subject, no two of one type.
    pub subjects: Vec<SubjectClaim>,
    /// The metric object.
    pub metric: MetricRef,
}

/// Deserializes a present field as `Some`, so that `null` is an error rather
/// than `None`. Pair with `#[serde(default)]` so an omitted field is `None`.
fn present_or_error<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Active-Quota counts behind the lifecycle gauges (PRD section 5.16):
/// `quota_cap_zero_total`, `quota_cap_unbounded_total`, and, joined with the
/// metric catalogue by the gear, `quota_for_direct_metric_total`.
/// Platform-plane and caller-less; only Quotas with lifecycle status `active`
/// count, whatever their validity window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActiveQuotaCounts {
    /// Active Quotas with `cap = 0`.
    pub cap_zero: u64,
    /// Active Quotas with an unbounded cap.
    pub cap_unbounded: u64,
    /// Active Quotas per metric.
    pub by_metric: HashMap<MetricId, u64>,
    /// The distinct non-empty `metadata` of the active Quotas of each metric,
    /// at most [`ActiveQuotaCounts::MAX_CONDITION_SETS`] of them: the gear
    /// checks them against the current metric type.
    #[serde(default)]
    pub conditions: Vec<ActiveConditions>,
}

impl ActiveQuotaCounts {
    /// The most condition sets one read returns. Conditions name attributes of
    /// a request, so the distinct sets of a metric are few; a deployment past
    /// this bound has the rest checked only when a Quota is written.
    pub const MAX_CONDITION_SETS: u64 = 1_000;
}

/// One distinct `metadata` of the active Quotas of a metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveConditions {
    /// The metric.
    pub metric: MetricId,
    /// The conditions.
    pub metadata: Map<String, Value>,
    /// Active Quotas that carry them.
    pub quotas: u64,
}

/// Optional validity bounds of a Quota. Both ends are inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidityWindow {
    /// Start of validity, inclusive. `None` means no lower bound.
    #[serde(default, with = "rfc3339::option")]
    pub start: Option<OffsetDateTime>,
    /// End of validity, inclusive. `None` means no upper bound.
    #[serde(default, with = "rfc3339::option")]
    pub end: Option<OffsetDateTime>,
}

impl ValidityWindow {
    /// True when `at` lies within the window.
    #[must_use]
    pub fn contains(&self, at: OffsetDateTime) -> bool {
        self.start.is_none_or(|s| s <= at) && self.end.is_none_or(|e| at <= e)
    }
}

// ---------------------------------------------------------------------------
// Quota
// ---------------------------------------------------------------------------

/// A stored Quota record (DESIGN section 3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "`quota_type` is the PRD field name; `type` is a keyword and `kind` would drift from the wire contract"
)]
pub struct Quota {
    /// Server-assigned identifier.
    pub id: QuotaId,
    /// PDP-authorized owning tenant.
    pub tenant_id: TenantId,
    /// Subject the Quota is bound to. Immutable after creation.
    pub subject: SubjectRef,
    /// Metric type. Immutable after creation.
    pub metric: MetricId,
    /// Accounting model. Immutable after creation.
    pub quota_type: QuotaType,
    /// Period specification. Present for consumption Quotas only.
    pub period: Option<PeriodType>,
    /// Behaviour at the cap boundary.
    pub enforcement_mode: EnforcementMode,
    /// Cap in metric units, within `0..=`[`Quota::MAX_CAP`]. `None` means
    /// unbounded.
    pub cap: Option<u64>,
    /// Notification thresholds as percentages of cap, ascending.
    pub notification_thresholds: Vec<u8>,
    /// Optional validity bounds.
    pub validity_window: Option<ValidityWindow>,
    /// Informational hint for callers: prefer fail-open when QE is unavailable.
    pub fail_open_hint: bool,
    /// The Quota's conditions over the `metadata` of a request's metric object:
    /// the Quota applies to a request only when all of them hold. QE checks
    /// them against the metric type when the Quota is written.
    pub metadata: Map<String, Value>,
    /// Who imposed the Quota.
    pub source: QuotaSource,
    /// Lifecycle state.
    pub status: QuotaStatus,
    /// Record version. Increments once per accepted mutation.
    pub record_version: u32,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last mutation time.
    #[serde(with = "rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Quota {
    /// Largest cap accepted by the API and signed 64-bit storage backends.
    pub const MAX_CAP: u64 = i64::MAX.unsigned_abs();
}

/// Public Quota read shape: the stored record plus computed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QuotaView {
    /// The stored record.
    #[serde(flatten)]
    pub quota: Quota,
    /// Whether the response clock lies within `validity_window`.
    pub currently_within_window: bool,
    /// Kind of the metric from its served catalogue entry at read time. `None`
    /// when the metric has no served entry; the record stays readable.
    #[serde(default)]
    pub metric_kind: Option<MetricKind>,
}

impl QuotaView {
    /// Pure computation of the view for the clock reading `now`.
    #[must_use]
    pub fn compute(quota: Quota, metric_kind: Option<MetricKind>, now: OffsetDateTime) -> Self {
        let currently_within_window = quota.validity_window.is_none_or(|w| w.contains(now));
        Self {
            quota,
            currently_within_window,
            metric_kind,
        }
    }
}

/// Public Quota create input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuotaSpec {
    /// PDP-authorized target tenant.
    pub tenant_id: TenantId,
    /// Subject the Quota binds to.
    pub subject: SubjectRef,
    /// Metric type.
    pub metric: MetricId,
    /// Accounting model. `rate` is reserved and rejected.
    pub quota_type: QuotaType,
    /// Period specification. Required for consumption Quotas, rejected for
    /// allocation Quotas.
    #[serde(default)]
    pub period: Option<PeriodType>,
    /// Behaviour at the cap boundary.
    pub enforcement_mode: EnforcementMode,
    /// Cap in metric units, within `0..=`[`Quota::MAX_CAP`]. `None` means
    /// unbounded.
    #[serde(default)]
    pub cap: Option<u64>,
    /// Notification thresholds as percentages of cap, ascending. Require a
    /// bounded cap.
    #[serde(default)]
    pub notification_thresholds: Vec<u8>,
    /// Optional validity bounds.
    #[serde(default)]
    pub validity_window: Option<ValidityWindow>,
    /// Informational fail-open hint. Defaults to fail-closed.
    #[serde(default)]
    pub fail_open_hint: bool,
    /// The Quota's conditions over the `metadata` of a request's metric object:
    /// the Quota applies to a request only when all of them hold. QE checks
    /// them against the metric type when the Quota is written.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// Who imposes the Quota.
    pub source: QuotaSource,
}

/// Create input for a Quota. The gear validates it before storage sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaDraft {
    /// PDP-authorized target tenant.
    pub tenant_id: TenantId,
    /// Admitted subject.
    pub subject: SubjectRef,
    /// Metric type.
    pub metric: MetricId,
    /// Accounting model.
    pub quota_type: QuotaType,
    /// Period specification. Consumption Quotas only.
    #[serde(default)]
    pub period: Option<PeriodType>,
    /// Behaviour at the cap boundary.
    pub enforcement_mode: EnforcementMode,
    /// Cap in metric units, within `0..=`[`Quota::MAX_CAP`]. `None` means
    /// unbounded.
    #[serde(default)]
    pub cap: Option<u64>,
    /// Notification thresholds as percentages of cap.
    #[serde(default)]
    pub notification_thresholds: Vec<u8>,
    /// Optional validity bounds.
    #[serde(default)]
    pub validity_window: Option<ValidityWindow>,
    /// Informational fail-open hint. Defaults to fail-closed.
    #[serde(default)]
    pub fail_open_hint: bool,
    /// The conditions, checked against the metric type.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// Who imposes the Quota.
    pub source: QuotaSource,
}

impl QuotaDraft {
    /// The storage draft of a validated `spec`.
    #[must_use]
    pub fn from_spec(spec: QuotaSpec) -> Self {
        Self {
            tenant_id: spec.tenant_id,
            subject: spec.subject,
            metric: spec.metric,
            quota_type: spec.quota_type,
            period: spec.period,
            enforcement_mode: spec.enforcement_mode,
            cap: spec.cap,
            notification_thresholds: spec.notification_thresholds,
            validity_window: spec.validity_window,
            fail_open_hint: spec.fail_open_hint,
            metadata: spec.metadata,
            source: spec.source,
        }
    }
}

/// Patch of a Quota's `cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapPatch {
    /// Set a numeric cap.
    Bounded(u64),
    /// Remove the cap.
    Unbounded,
}

/// Patch of a Quota's `validity_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValidityWindowPatch {
    /// Remove the window.
    Clear,
    /// Replace the window.
    Set(ValidityWindow),
}

/// A caller's partial update of a Quota, the client and REST shape. Absent
/// fields stay untouched. Metric, type, period, and subject are immutable and
/// therefore absent here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaUpdate {
    /// New cap.
    pub cap: Option<CapPatch>,
    /// New thresholds, replacing the previous list.
    pub notification_thresholds: Option<Vec<u8>>,
    /// New validity window.
    pub validity_window: Option<ValidityWindowPatch>,
    /// New metadata object, replacing the previous one.
    pub metadata: Option<Map<String, Value>>,
    /// New enforcement mode.
    pub enforcement_mode: Option<EnforcementMode>,
    /// New fail-open hint.
    pub fail_open_hint: Option<bool>,
}

/// A validated partial update of a Quota as storage applies it, built by the
/// gear from a [`QuotaUpdate`]. Absent fields stay untouched.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaPatch {
    /// New cap.
    pub cap: Option<CapPatch>,
    /// New thresholds, replacing the previous list.
    pub notification_thresholds: Option<Vec<u8>>,
    /// New validity window.
    pub validity_window: Option<ValidityWindowPatch>,
    /// New metadata object, replacing the previous one.
    pub metadata: Option<Map<String, Value>>,
    /// New enforcement mode.
    pub enforcement_mode: Option<EnforcementMode>,
    /// New fail-open hint.
    pub fail_open_hint: Option<bool>,
}

impl QuotaPatch {
    /// True when the patch changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Filter for [`crate::QuotaEnforcementStoragePluginV1::read_quotas`].
/// Every set field narrows the result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct QuotaFilter {
    /// Owning tenant.
    pub tenant_id: Option<TenantId>,
    /// Bound subject.
    pub subject: Option<SubjectRef>,
    /// Metric.
    pub metric: Option<MetricId>,
    /// Lifecycle state.
    pub status: Option<QuotaStatus>,
    /// Explicit identifiers. Empty means no restriction.
    pub ids: Vec<QuotaId>,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Cursor-based page request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PageRequest {
    /// Maximum number of items. Storage may clamp it to its own maximum.
    pub limit: u32,
    /// Opaque continuation cursor from a previous [`PageResult`].
    #[serde(default)]
    pub cursor: Option<String>,
}

impl PageRequest {
    /// Platform default page size.
    pub const DEFAULT_LIMIT: u32 = 100;

    /// First page with the given limit.
    #[must_use]
    pub const fn first(limit: u32) -> Self {
        Self {
            limit,
            cursor: None,
        }
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self::first(Self::DEFAULT_LIMIT)
    }
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResult<T> {
    /// Items of this page.
    pub items: Vec<T>,
    /// Cursor for the next page. `None` on the last page.
    pub next_cursor: Option<String>,
}

// Named per item type: the registry refuses two pages under one schema name.
impl<T: utoipa::ToSchema> utoipa::PartialSchema for PageResult<T> {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Type};
        use utoipa::openapi::{Ref, RefOr};

        ObjectBuilder::new()
            .property(
                "items",
                ArrayBuilder::new().items(RefOr::<utoipa::openapi::schema::Schema>::Ref(
                    Ref::from_schema_name(T::name().to_string()),
                )),
            )
            .required("items")
            .property(
                "next_cursor",
                ObjectBuilder::new().schema_type(utoipa::openapi::schema::SchemaType::from_iter([
                    Type::String,
                    Type::Null,
                ])),
            )
            .into()
    }
}

impl<T: utoipa::ToSchema> utoipa::ToSchema for PageResult<T> {
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!("PageResult_{}", T::name()))
    }

    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        schemas.push((T::name().to_string(), T::schema()));
        T::schemas(schemas);
    }
}

impl<T> PageResult<T> {
    /// An empty final page.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
        }
    }

    /// Map every item, keeping the cursor.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> PageResult<U> {
        PageResult {
            items: self.items.into_iter().map(f).collect(),
            next_cursor: self.next_cursor,
        }
    }
}

impl<T> Default for PageResult<T> {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// The admitted subject set of one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicableQuotas {
    /// Authorized target tenant.
    pub tenant_id: TenantId,
    /// Every applicable subject, the tenant subject included.
    pub subjects: Vec<SubjectRef>,
    /// The operation's metric.
    pub metric: MetricId,
}

/// Whether every condition of a Quota's `metadata` holds on `request`, the
/// `metadata` of the request's metric object (ADR-0013): the request has the
/// property, and its value equals the condition's value or one of its values.
/// The empty object has no conditions.
#[must_use]
pub fn conditions_hold(conditions: &Map<String, Value>, request: &Value) -> bool {
    conditions.iter().all(|(property, admitted)| {
        request.get(property).is_some_and(|value| match admitted {
            Value::Array(values) => values.contains(value),
            admitted => admitted == value,
        })
    })
}

/// One condition of Quota metadata that does not fit the metric type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaMetadataViolation {
    /// JSON pointer into the metadata; empty for the metadata as a whole.
    pub path: String,
    /// What is wrong there.
    pub reason: String,
}

/// One entry of a [`DebitChain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QuotaDebitPlan {
    /// The charged Quota.
    pub quota_id: QuotaId,
    /// Counter mutation for the Quota. Never negative.
    pub amount: u64,
}

/// Entries of one chain, primary pool first. Their amounts add up to exactly
/// the requested amount: a cascade or a split.
pub type DebitChain = Vec<QuotaDebitPlan>;

/// Chains of a [`Decision`]. Several chains are an AND: each counts the whole
/// operation. A Quota appears at most once in a plan.
pub type DebitPlan = Vec<DebitChain>;

/// Rebuild a plan from entries tagged with their `(chain, position)`; chains
/// and entries keep the order of those indices.
#[must_use]
pub fn plan_of(entries: impl IntoIterator<Item = ((u32, u32), QuotaDebitPlan)>) -> DebitPlan {
    let mut chains: BTreeMap<u32, BTreeMap<u32, QuotaDebitPlan>> = BTreeMap::new();
    for ((chain, position), entry) in entries {
        chains.entry(chain).or_default().insert(position, entry);
    }
    chains
        .into_values()
        .map(|chain| chain.into_values().collect())
        .collect()
}

/// Every entry of `plan` with its `(chain, position)`, in plan order.
pub fn plan_entries(plan: &DebitPlan) -> impl Iterator<Item = ((u32, u32), &QuotaDebitPlan)> {
    plan.iter().zip(0_u32..).flat_map(|(chain, chain_index)| {
        chain
            .iter()
            .zip(0_u32..)
            .map(move |(entry, position)| ((chain_index, position), entry))
    })
}

/// Two-arm verdict of an evaluation (PRD section 3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DecisionResult {
    /// The operation is within every applicable cap.
    Allowed,
    /// At least one Quota would be exceeded. Counters are unchanged.
    Denied {
        /// Every violating Quota. Empty when no Quota applied at all.
        violated_quota_ids: Vec<QuotaId>,
        /// Closed reason token, for example `NO_APPLICABLE_QUOTA`.
        reason: String,
    },
}

/// Engine output. Server-derived, response-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Decision {
    /// The verdict.
    pub result: DecisionResult,
    /// Mutation plan, one list per chain. Empty when the result is `Denied`.
    // Spelled out: utoipa names a schema after the alias's outer type, and a
    // component named `Vec` collides with any other gear's.
    #[serde(default)]
    #[schema(value_type = Vec<Vec<QuotaDebitPlan>>)]
    pub debit_plan: DebitPlan,
    /// Engine-supplied per-Quota detail.
    #[serde(default)]
    pub diagnostics: BTreeMap<String, Value>,
}

/// Denial reason of an operation whose metric no active Quota covers. The one
/// denial that persists nothing (PRD section 5.5).
pub const NO_APPLICABLE_QUOTA: &str = "NO_APPLICABLE_QUOTA";

impl Decision {
    /// An allowed decision carrying `plan` and no diagnostics.
    #[must_use]
    pub fn allowed_with_plan(plan: DebitPlan) -> Self {
        Self {
            result: DecisionResult::Allowed,
            debit_plan: plan,
            diagnostics: BTreeMap::new(),
        }
    }

    /// The denial reason, or `None` when the decision allowed the operation.
    #[must_use]
    pub fn denied_reason(&self) -> Option<&str> {
        self.result.denied_reason()
    }

    /// Whether this decision is the denial that records nothing.
    #[must_use]
    pub fn is_no_applicable_quota(&self) -> bool {
        self.denied_reason() == Some(NO_APPLICABLE_QUOTA)
    }
}

impl DecisionResult {
    /// The denial reason, or `None` for [`Self::Allowed`].
    #[must_use]
    pub fn denied_reason(&self) -> Option<&str> {
        match self {
            Self::Allowed => None,
            Self::Denied { reason, .. } => Some(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Consumer requests
// ---------------------------------------------------------------------------

// None of the four request shapes rejects unknown fields. A caller that echoes
// a Decision back into its next request must be ignored, not failed
// (PRD section 3.4), and serde has no ignore-only-these mode. The nested
// `attribution` keeps its own strictness, so a misspelled attribution field is
// still an error.

/// Charge a metric against every Quota that applies to the attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DebitRequest {
    /// Who and what is being charged.
    pub attribution: EvaluationAttribution,
    /// Requested amount. Signed on the wire so that zero and negative values
    /// reach the domain as `INVALID_AMOUNT` rather than failing
    /// deserialization with an unactionable 422.
    pub amount: i64,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Return consumption to one Quota. Corrective and operator-facing: it names a
/// Quota by id and evaluates no policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreditRequest {
    /// Authorized target tenant.
    pub tenant_id: TenantId,
    /// The Quota to credit.
    pub quota_id: QuotaId,
    /// Amount to return. See [`DebitRequest::amount`] for the signedness.
    pub amount: i64,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Return everything that remains of a charge (ADR-0011).
///
/// The attribution is the charged operation's, not a new one: the server
/// authorizes it for the action `rollback` and requires the charge to carry
/// the same attribution digest. A rollback has no amount and no key of its
/// own; on a charge with nothing left it succeeds and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RollbackRequest {
    /// Attribution of the charged operation.
    pub attribution: EvaluationAttribution,
    /// Which kind of operation the key names. Defaults to a direct debit.
    #[serde(default)]
    pub original_operation: RollbackableOperation,
    /// Idempotency key the charged operation was committed under.
    pub original_idempotency_key: String,
}

/// Evaluate without mutating anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PreviewRequest {
    /// Who and what would be charged.
    pub attribution: EvaluationAttribution,
    /// Amount to test. See [`DebitRequest::amount`] for the signedness.
    pub amount: i64,
}

/// Hold capacity against every applicable Quota for a bounded TTL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AcquireLeaseRequest {
    /// Who and what is being held.
    pub attribution: EvaluationAttribution,
    /// Amount to hold. See [`DebitRequest::amount`] for the signedness.
    pub amount: i64,
    /// Requested lifetime. Absent or outside the operator's
    /// `[min_lease_ttl, max_lease_ttl]` window is `TTL_OUT_OF_BOUNDS`; the
    /// value is never clamped, because the holder is entitled to exactly the
    /// TTL it reserved.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Convert an active lease into a debit for what was actually used.
///
/// The tenant is carried explicitly, as [`CreditRequest`] does, so the
/// authenticated principal is authorized before the token is ever read: a token
/// outside the caller's authorized tenant is simply absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CommitLeaseRequest {
    /// Authorized target tenant.
    pub tenant_id: TenantId,
    /// The lease to settle.
    pub token: LeaseToken,
    /// What was actually used, defaulting to the full reserved amount. Zero
    /// commits nothing and returns every hold; negative is `INVALID_AMOUNT`;
    /// above the reserved amount is `OVER_COMMIT_NOT_AUTHORIZED`.
    #[serde(default)]
    pub actual_amount: Option<i64>,
    /// The metric type id the amount is for. When present it must be the
    /// lease's, or the commit is `METRIC_TYPE_MISMATCH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_type: Option<String>,
    /// Client-supplied idempotency key of this commit.
    pub idempotency_key: String,
}

/// Return an active lease's held capacity without committing a debit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReleaseLeaseRequest {
    /// Authorized target tenant. See [`CommitLeaseRequest::tenant_id`].
    pub tenant_id: TenantId,
    /// The lease to release.
    pub token: LeaseToken,
    /// Client-supplied idempotency key of this release.
    pub idempotency_key: String,
}

/// How a batch debit's items relate to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchMode {
    /// All or nothing: every item is admitted, or none moves a counter.
    Atomic,
    /// Partial success for unrelated items. Not implemented in P1; refused
    /// with `NOT_YET_IMPLEMENTED`.
    Independent,
}

/// One item of a batch debit: a debit of its own metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BatchItemRequest {
    /// Who and what this item charges.
    pub attribution: EvaluationAttribution,
    /// Requested amount. Signed so a non-positive value is refused as
    /// `INVALID_AMOUNT` naming this item, rather than failing to decode.
    pub amount: i64,
    /// The item's own key, for identification only: it is part of the
    /// envelope's payload and echoed in the outcome, and has no replay of its
    /// own.
    pub idempotency_key: String,
}

/// Several debits of one logical operation, admitted or refused as a whole.
///
/// The batch must not be empty (`BATCH_EMPTY`), and every item must name the
/// same tenant (`BATCH_TENANT_MIXED`): the envelope has one idempotency scope,
/// keyed by that tenant. Each item is still authorized on its own attribution.
/// Item keys must be unique within the batch (`BATCH_ITEM_KEY_DUPLICATE`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BatchDebitRequest {
    /// Required batch semantics.
    pub mode: BatchMode,
    /// The items, evaluated in this order.
    pub items: Vec<BatchItemRequest>,
    /// Envelope idempotency key: a replay returns the original outcome.
    pub idempotency_key: String,
}

/// The batch-level verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchResult {
    /// Every item was allowed and the union of their plans was applied.
    Allowed,
    /// At least one item was denied, and no counter moved.
    Denied,
}

/// One item's decision, in submission order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BatchItemOutcome {
    /// The item's own key.
    pub idempotency_key: String,
    /// The item's decision. On a denied batch the decisions are diagnostic:
    /// nothing was applied, allowed items included.
    pub decision: Decision,
}

/// The outcome of an atomic batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[must_use]
pub struct BatchDecision {
    /// The batch-level verdict.
    pub result: BatchResult,
    /// One outcome per item, in submission order.
    pub items: Vec<BatchItemOutcome>,
}

impl BatchDecision {
    /// The batch outcome of per-item `decisions`, paired in order with the
    /// items' `keys`: denied when any item is.
    pub fn of(keys: impl IntoIterator<Item = String>, decisions: Vec<Decision>) -> Self {
        let result = if decisions
            .iter()
            .all(|decision| matches!(decision.result, DecisionResult::Allowed))
        {
            BatchResult::Allowed
        } else {
            BatchResult::Denied
        };
        Self {
            result,
            items: keys
                .into_iter()
                .zip(decisions)
                .map(|(idempotency_key, decision)| BatchItemOutcome {
                    idempotency_key,
                    decision,
                })
                .collect(),
        }
    }
}

/// What a batch envelope's idempotency record stores: one decision per item,
/// in submission order, under a schema version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    /// Schema version of this record.
    #[serde(rename = "__version")]
    pub version: u32,
    /// Every item's decision.
    pub decisions: Vec<Decision>,
}

impl BatchRecord {
    /// The current schema version.
    pub const VERSION: u32 = 1;

    /// A record of `decisions` under the current version.
    #[must_use]
    pub fn new(decisions: Vec<Decision>) -> Self {
        Self {
            version: Self::VERSION,
            decisions,
        }
    }
}

/// A decision that was never applied. Structurally a [`Decision`] plus the flag
/// that says so, so a caller can parse both responses the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DecisionPreview {
    /// The verdict, flattened: `result`, `debit_plan`, and `diagnostics` sit at
    /// the top level next to `preview`.
    #[serde(flatten)]
    pub decision: Decision,
    /// Always `true`. Present so a dry run can never be mistaken for a commit.
    pub preview: bool,
}

impl DecisionPreview {
    /// Mark `decision` as a dry run.
    #[must_use]
    pub const fn of(decision: Decision) -> Self {
        Self {
            decision,
            preview: true,
        }
    }
}

/// The one conversion site between the wire's signed amount and the unsigned
/// amount every counter uses. `None` for zero and negatives, which the domain
/// reports as `INVALID_AMOUNT`.
#[must_use]
pub const fn positive_amount(amount: i64) -> Option<u64> {
    if amount <= 0 {
        None
    } else {
        // Non-negative after the guard, so the conversion is exact.
        Some(amount.unsigned_abs())
    }
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

/// Idempotency scope: `(tenant_id, operation_type, key)`. The subjects are
/// part of the request fingerprint, not of the scope, so a key is taken for
/// the whole tenant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyScope {
    /// Authorized target tenant.
    pub tenant_id: TenantId,
    /// Operation kind.
    pub operation_type: OperationType,
    /// Client-supplied key.
    pub key: String,
}

/// The caller's half of an idempotency record: the key and the digest a replay
/// is compared against. What is recorded under it — the decision and the policy
/// that produced it — is the transaction's own, never the caller's, so a replay
/// returns what actually happened (invariants I1 and I2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyWrite {
    /// Full scope.
    pub scope: IdempotencyScope,
    /// Canonical payload digest for replay comparison.
    pub payload_hash: PayloadHash,
}

/// A stored idempotency record. For an allowed debit or a lease commit it is
/// read from the charge, which is the record of that operation for as long as
/// the charge lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    /// Full scope.
    pub scope: IdempotencyScope,
    /// Canonical payload digest.
    pub payload_hash: PayloadHash,
    /// Schema-versioned decision blob (top-level `__version`).
    pub decision_blob: Value,
    /// Engine that produced the decision. Absent for a primitive that
    /// evaluates no policy, such as an operator credit.
    pub engine_id: Option<String>,
    /// Policy that produced the decision, when one was selected.
    pub policy_id: Option<PolicyId>,
    /// Version of that policy, when one was selected.
    pub policy_version: Option<u32>,
    /// Record creation time.
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    /// Retention deadline; for a charge, the end of its return window, which
    /// it lives at least until.
    #[serde(with = "rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl IdempotencyRecord {
    /// The recorded decision, decoded from the schema-versioned blob. The
    /// top-level `__version` key is ignored by [`Decision`]'s deserializer, so
    /// an additive P2 or P3 blob still decodes here.
    ///
    /// # Errors
    ///
    /// Returns the deserialization error when the blob is not a Decision.
    pub fn decision(&self) -> Result<Decision, serde_json::Error> {
        serde_json::from_value(self.decision_blob.clone())
    }
}

/// Schema version written as the `__version` key of a stored decision blob.
pub const DECISION_BLOB_VERSION: u32 = 1;

/// The caller's half of an idempotency record whose operation type the
/// primitive fixes: a credit, and a lease commit or release, which share one
/// settlement path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialIdempotencyWrite {
    /// Authorized target tenant.
    pub tenant_id: TenantId,
    /// Client-supplied key.
    pub key: String,
    /// Canonical payload digest for replay comparison.
    pub payload_hash: PayloadHash,
}

impl PartialIdempotencyWrite {
    /// The full write under `operation_type`.
    #[must_use]
    pub fn complete(self, operation_type: OperationType) -> IdempotencyWrite {
        IdempotencyWrite {
            scope: IdempotencyScope {
                tenant_id: self.tenant_id,
                operation_type,
                key: self.key,
            },
            payload_hash: self.payload_hash,
        }
    }
}

/// The charge a rollback reverses.
///
/// The scope locates the charge; the digest proves the caller was authorized
/// for that charge's attribution, not merely for some operation of the
/// tenant. A charge whose attribution digest differs is reported absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackTarget {
    /// Scope of the charged operation. Its `operation_type` is
    /// [`OperationType::Debit`] or [`OperationType::Commit`], as the caller's
    /// [`RollbackableOperation`] named it; the plugin looks up that one scope
    /// and never falls back to the other.
    pub original: IdempotencyScope,
    /// Digest of the attribution the rollback caller was authorized under,
    /// which must equal the charge's.
    pub authorized: AttributionDigest,
}

/// A return as storage applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeReturn {
    /// Scope of the charged operation, as in [`RollbackTarget::original`].
    pub charge: IdempotencyScope,
    /// The metric type the amount is counted in.
    pub metric: MetricId,
    /// Positive amount to give back.
    pub amount: u64,
    /// The return's own key; its scope is `(tenant, return, key)`.
    pub key: String,
}

impl ChargeReturn {
    /// The idempotency scope of the return's own key.
    #[must_use]
    pub fn scope(&self) -> IdempotencyScope {
        IdempotencyScope {
            tenant_id: self.charge.tenant_id,
            operation_type: OperationType::Return,
            key: self.key.clone(),
        }
    }
}

/// The allocation charges of one metric that hold an amount on an active
/// Quota past their return window (ADR-0011).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldAllocationCharges {
    /// The charges' metric.
    pub metric: MetricId,
    /// How many there are.
    pub count: u64,
    /// Creation time of the oldest.
    #[serde(with = "rfc3339")]
    pub oldest_created_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Events and mutation results
// ---------------------------------------------------------------------------

/// Ownership of an event. Platform policy events have no tenant identity.
/// Tenant events retain their top-level `tenant_id` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotificationScope {
    /// A tenant-owned Quota, lease, or consumption event.
    Tenant {
        /// Owning tenant.
        tenant_id: TenantId,
    },
    /// A platform-wide policy event, independent of the operator's tenant.
    Platform,
}

/// The identity the operation log and the events name as the cause of an
/// operation. It is recorded, never authorized by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Actor {
    /// The authenticated principal of a call through QE's own API.
    Caller {
        /// The principal's subject id.
        subject_id: Uuid,
    },
    /// The platform identity of the quota resolver's caller, as the resolver
    /// plugin request carries it. `None` for a caller in the resolver's
    /// process, which the platform gives no identity.
    Platform {
        /// The caller's platform identity, when it has one.
        identity: Option<toolkit_security::PlatformIdentity>,
    },
    /// Work that no caller started.
    Task {
        /// The task.
        task: BackgroundTask,
    },
}

/// A background task acting without a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundTask {
    /// The lease sweeper, which releases expired leases.
    LeaseSweeper,
    /// The gear's bootstrap, which seeds the `global` Policy.
    Bootstrap,
}

impl BackgroundTask {
    /// The task's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LeaseSweeper => "lease-sweeper",
            Self::Bootstrap => "bootstrap",
        }
    }
}

impl fmt::Display for Actor {
    /// The record form: the subject id of a caller, `platform:<peer name>` or
    /// `platform` for the resolver's caller, the name of a task.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caller { subject_id } => write!(f, "{subject_id}"),
            Self::Platform {
                identity: Some(identity),
            } => write!(f, "platform:{}", identity.peer_name()),
            Self::Platform { identity: None } => f.write_str("platform"),
            Self::Task { task } => f.write_str(task.name()),
        }
    }
}

/// Same-transaction outbox event (invariant I11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEvent {
    /// Unique event identifier. Sinks deduplicate on it.
    pub event_id: EventId,
    /// Catalog kind.
    pub kind: NotificationEventKind,
    /// The cause of the operation that enqueued the event.
    pub actor: Actor,
    /// Explicit ownership scope; platform events never carry a synthetic tenant.
    #[serde(flatten)]
    pub scope: NotificationScope,
    /// Target Quota, when applicable.
    #[serde(default)]
    pub quota_id: Option<QuotaId>,
    /// Target Policy, when applicable.
    #[serde(default)]
    pub policy_id: Option<PolicyId>,
    /// Subject, when applicable.
    #[serde(default)]
    pub subject: Option<SubjectRef>,
    /// Event-specific payload.
    pub payload: Value,
    /// Emission time.
    #[serde(with = "rfc3339")]
    pub emitted_at: OffsetDateTime,
}

/// Post-mutation counter value of one Quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterSnapshot {
    /// The Quota.
    pub quota_id: QuotaId,
    /// The period row for consumption Quotas.
    pub period_id: Option<PeriodId>,
    /// Consumed or in-flight amount after the mutation.
    pub value: u64,
}

/// Upward threshold transition observed by a mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThresholdCrossing {
    /// The Quota.
    pub quota_id: QuotaId,
    /// Every threshold the mutation crossed, ascending.
    pub crossed_thresholds: Vec<u8>,
    /// Maximum of `crossed_thresholds`.
    pub highest_crossed_threshold: u8,
}

/// Return value of every mutating storage primitive.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MutationResult {
    /// New counter values.
    pub counters: Vec<CounterSnapshot>,
    /// Threshold crossings the mutation produced.
    pub threshold_crossings: Vec<ThresholdCrossing>,
    /// Identifiers of the events enqueued in the same transaction.
    pub event_ids: Vec<EventId>,
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// One per-Quota hold of a lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseHold {
    /// The held Quota.
    pub quota_id: QuotaId,
    /// Held amount.
    pub held_amount: u64,
    /// Acquisition period row for consumption Quotas (invariant I5).
    pub period_id: Option<PeriodId>,
}

/// An expired lease reclaimed by the sweeper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiredLease {
    /// The lease token.
    pub token: LeaseToken,
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Holds returned by the auto-release.
    pub holds: Vec<LeaseHold>,
    /// Expiry time.
    #[serde(with = "rfc3339")]
    pub expired_at: OffsetDateTime,
}

/// Item of a batch-debit envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchDebitItem {
    /// The item's applicable subject set.
    pub applicable: ApplicableQuotas,
    /// The item's requested amount. The plan is the transaction's to compute.
    pub amount: u64,
    /// The validated `metadata` of the item's metric object.
    pub request: Value,
    /// The `id` of the item's metric object, for the Engine.
    pub metric_id: Option<String>,
    /// Digest of the attribution this item was authorized under, which its
    /// charge keeps.
    pub authorized: AttributionDigest,
    /// The item's own key in the key space of single debits, `(tenant, debit,
    /// key)`, with the fingerprint of a single debit of the item's request.
    pub idempotency: IdempotencyWrite,
}

/// What an evaluated mutation committed: the decision the transaction's own
/// engine invocation produced, and the counters the applied plan left behind.
/// A denial commits no counter change and still occupies the idempotency key,
/// so replaying a denial denies again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedDebit {
    /// The decision the transaction evaluated, validated before any mutation.
    pub decision: Decision,
    /// Counter state after the plan. Empty on a denial, and empty on a replay:
    /// a replay moves nothing, and the stored decision is the whole answer.
    pub mutation: MutationResult,
    /// Whether an idempotency record now holds this outcome, and until when.
    pub retention: Retention,
}

/// Whether a mutating primitive recorded its outcome under the idempotency
/// key, and how long a replay will find it.
///
/// A denial with reason [`NO_APPLICABLE_QUOTA`] persists nothing at all
/// (no record, no audit row), because provisioning a Quota must change the
/// answer rather than replay the denial. Every other outcome is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// A record holds this outcome until `expires_at`. A caching replica must
    /// not answer from memory past that instant: the database treats the key
    /// as new from then on.
    Recorded {
        /// Retention deadline of the record.
        #[serde(with = "rfc3339")]
        expires_at: OffsetDateTime,
    },
    /// Nothing was persisted. The same request re-evaluates from scratch and
    /// must not be cached.
    Unrecorded,
}

impl Retention {
    /// The retention deadline, or `None` when nothing was recorded.
    #[must_use]
    pub const fn expires_at(&self) -> Option<OffsetDateTime> {
        match self {
            Self::Recorded { expires_at } => Some(*expires_at),
            Self::Unrecorded => None,
        }
    }
}

/// What a corrective mutation committed: credit, return and rollback, which
/// evaluate no policy and are always allowed once their guards pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedMutation {
    /// The recorded decision. Always [`DecisionResult::Allowed`]; its plan
    /// carries the amounts moved.
    pub decision: Decision,
    /// Counter state after the mutation.
    pub mutation: MutationResult,
}

/// What an evaluated lease acquisition committed. A denied acquisition holds
/// nothing and carries no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedLease {
    /// The decision the transaction evaluated.
    pub decision: Decision,
    /// The acquired lease, absent when the decision denied the operation.
    pub token: Option<LeaseToken>,
    /// When the acquired lease expires. Present exactly when `token` is.
    #[serde(default, with = "rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

/// What an acquisition returned to its caller: a held lease, or the verdict
/// that refused to hold one.
///
/// A denial is a successful call carrying a [`Decision`], not an error
/// (PRD section 3.4), which a bare token could not express.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[must_use]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AcquireLeaseOutcome {
    /// Capacity is held on every Quota of the plan until `expires_at`.
    Acquired {
        /// Opaque, server-issued lease token.
        token: LeaseToken,
        /// When the hold lapses without a commit or release.
        #[serde(with = "rfc3339")]
        expires_at: OffsetDateTime,
    },
    /// No capacity was held on any Quota.
    Denied {
        /// The verdict, with the reason and the violated Quotas.
        decision: Decision,
    },
}

impl AcquireLeaseOutcome {
    /// Read the storage outcome as the caller-facing answer.
    ///
    /// # Errors
    ///
    /// Returns the evaluated lease back when it allowed the acquisition but
    /// carries no token or no expiry, which no implementation may produce.
    pub fn of(evaluated: EvaluatedLease) -> Result<Self, Box<EvaluatedLease>> {
        match (evaluated.token, evaluated.expires_at) {
            (Some(token), Some(expires_at)) => Ok(Self::Acquired { token, expires_at }),
            (None, _) if !matches!(evaluated.decision.result, DecisionResult::Allowed) => {
                Ok(Self::Denied {
                    decision: evaluated.decision,
                })
            }
            _ => Err(Box::new(evaluated)),
        }
    }
}

/// What one entry of a charge or of a lease's holds still holds, at the chain
/// and position its Debit Plan gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldEntry {
    /// Index of the entry's chain in the plan.
    pub chain: u32,
    /// Index of the entry inside its chain, primary pool first.
    pub position: u32,
    /// What the entry still holds.
    pub held: u64,
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-unwind:p1
/// Take `amount` back from every chain of `entries`: in each chain from the
/// last entry back, each entry at most what it still holds, so each chain
/// gives back exactly `amount` (ADR-0010).
///
/// Returns the amount taken from each entry, in the order of `entries`.
///
/// # Errors
///
/// [`UnwindError`] naming the first chain that holds less than `amount`.
pub fn unwind(entries: &[HeldEntry], amount: u64) -> Result<Vec<u64>, UnwindError> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-unwind:p1:inst-unw-chain
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&index| {
        (
            entries[index].chain,
            std::cmp::Reverse(entries[index].position),
        )
    });
    let mut taken = vec![0; entries.len()];
    for chain in order.chunk_by(|&left, &right| entries[left].chain == entries[right].chain) {
        let mut left = amount;
        for &index in chain {
            let take = left.min(entries[index].held);
            taken[index] = take;
            left -= take;
        }
        if left > 0 {
            return Err(UnwindError {
                chain: entries[chain[0]].chain,
            });
        }
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-unwind:p1:inst-unw-chain
    // @cpt-begin:cpt-cf-quota-enforcement-algo-unwind:p1:inst-unw-return
    Ok(taken)
    // @cpt-end:cpt-cf-quota-enforcement-algo-unwind:p1:inst-unw-return
}

/// A chain holds less than the amount to unwind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("chain {chain} holds less than the amount to unwind")]
pub struct UnwindError {
    /// The chain that falls short.
    pub chain: u32,
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// Current period bounds of a consumption Quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PeriodWindow {
    /// Period start, inclusive.
    #[serde(with = "rfc3339")]
    pub start: OffsetDateTime,
    /// Period end, exclusive.
    #[serde(with = "rfc3339")]
    pub end: OffsetDateTime,
    /// Next reset time.
    #[serde(with = "rfc3339")]
    pub next_reset: OffsetDateTime,
}

/// Per-Quota state returned by snapshot reads (PRD section 5.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QuotaSnapshot {
    /// The Quota.
    pub quota_id: QuotaId,
    /// Bound subject.
    pub subject: SubjectRef,
    /// Metric.
    pub metric: MetricId,
    /// Accounting model.
    pub quota_type: QuotaType,
    /// Behaviour at the cap boundary.
    pub enforcement_mode: EnforcementMode,
    /// Cap. `None` means unbounded.
    pub cap: Option<u64>,
    /// Consumed amount, or in-flight amount for allocation Quotas.
    pub consumed: u64,
    /// Remaining capacity. `None` when the cap is unbounded.
    pub remaining: Option<u64>,
    /// Period bounds for consumption Quotas.
    pub period: Option<PeriodWindow>,
    /// Operator metadata.
    pub metadata: Map<String, Value>,
    /// Validity bounds.
    pub validity_window: Option<ValidityWindow>,
    /// Server-computed: the snapshot time lies within the validity window.
    pub currently_within_window: bool,
}

/// One target of a snapshot read: a subject type, its identifier, and a
/// metric type, all as text.
///
/// A filter describes a target, not an evaluation claim. A subject of any
/// type but the tenant's selects the active Quotas on the metric whose subject
/// is that subject or the request's tenant. A tenant-type subject is a
/// tenant-only filter: its `id` must equal the request's `tenant_id`, and it
/// selects the tenant's own Quotas only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSubject {
    /// A quota subject type id, as text.
    #[serde(rename = "type")]
    pub subject_type: String,
    /// Opaque, non-empty subject identifier.
    pub id: String,
    /// The metric type id, as text.
    pub metric: String,
    /// The `metadata` of a request's metric object. When present, the target
    /// selects only the Quotas whose conditions hold on it, the set a debit
    /// with this `metadata` is checked against; it must fit the metric type.
    /// When absent, the target selects the Quotas whatever their conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// Read the per-Quota state of `1..N` explicit targets in one tenant.
///
/// Single and bulk reads are one request shape: one subject is the single
/// read. The PDP authorizes the complete target, every filter included, before
/// anything is mapped or read, so a deployment's `snapshot`/`read` policy must
/// inspect each filter: one filter outside the caller's grant denies the whole
/// request. The union of the filters returns each active Quota once, ordered
/// by `quota_id`, including Quotas outside their validity window. A page may
/// hold fewer Quotas than `limit` when a target carries `metadata`; the cursor
/// says whether more follow.
///
/// Refused with `InvalidArgument` before the PDP: no subjects
/// (`SNAPSHOT_SUBJECTS_REQUIRED`), more than the operator allows
/// (`SNAPSHOT_TOO_MANY_SUBJECTS`), a `limit` outside `1..=page_size`
/// (`SNAPSHOT_LIMIT_OUT_OF_RANGE`), or a `tenant` filter naming another tenant
/// (`SNAPSHOT_TENANT_MISMATCH`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotRequest {
    /// Target tenant.
    pub tenant_id: TenantId,
    /// The targets.
    pub subjects: Vec<SnapshotSubject>,
    /// Page size; the operator's page size when absent, and never above it.
    pub limit: Option<u32>,
    /// Continuation cursor from the previous page.
    pub cursor: Option<String>,
}

/// Outcome of a Quota deactivation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeactivateOutcome {
    /// Leases resolved atomically with the deactivation.
    pub resolved_leases: Vec<LeaseToken>,
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

/// Scope of a Quota Resolution Policy. P1 is closed to two levels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyScope {
    /// Platform-wide fallback.
    Global,
    /// One metric.
    Metric {
        /// The metric.
        metric: MetricId,
    },
}

/// Whether a policy transition actually moved state, or replayed a settled one.
///
/// A replayed rollback onto the already-active target, or a repeated delete of
/// an already-deleted policy, commits no audit row and enqueues no event. The
/// caller must therefore not count a `policy_version_transitions_total` for it,
/// which is why the distinction is a variant rather than a flag: matching is
/// the only way to reach the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionOutcome<T> {
    /// State moved; the audit row and events committed with it.
    Applied(T),
    /// Nothing changed; no audit row and no event were written.
    NoOp(T),
}

impl<T> TransitionOutcome<T> {
    /// The value either way, discarding whether it moved.
    pub fn into_inner(self) -> T {
        match self {
            Self::Applied(value) | Self::NoOp(value) => value,
        }
    }

    /// Borrow the value either way.
    pub const fn get(&self) -> &T {
        match self {
            Self::Applied(value) | Self::NoOp(value) => value,
        }
    }

    /// Whether state actually moved, and so whether a transition may be counted.
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

/// Public create input. Actor identity and validation snapshots are server-owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicySpec {
    /// Scope.
    pub scope: PolicyScope,
    /// Registered engine identifier.
    pub engine_id: String,
    /// Engine-validated configuration.
    pub engine_config: Value,
    /// Per-policy evaluation timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Operator description.
    #[serde(default)]
    pub description: Option<String>,
    /// Version comment.
    #[serde(default)]
    pub comment: Option<String>,
}

/// Public update input. Creates a new immutable version after operator admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    /// Expected current version. Lost-update protection.
    pub if_match_version: u32,
    /// New engine identifier.
    #[serde(default)]
    pub engine_id: Option<String>,
    /// New engine configuration.
    #[serde(default)]
    pub engine_config: Option<Value>,
    /// New timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Version comment.
    #[serde(default)]
    pub comment: Option<String>,
}

/// Create input for a Policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDraft {
    /// Server-resolved engine schema closure; empty for MRW.
    #[serde(default)]
    pub schema_snapshot: crate::engine::PolicySchemaSnapshot,
    /// Scope.
    pub scope: PolicyScope,
    /// Registered engine identifier.
    pub engine_id: String,
    /// Engine-validated configuration.
    pub engine_config: Value,
    /// Per-policy evaluation timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Operator description.
    #[serde(default)]
    pub description: Option<String>,
    /// Version comment.
    #[serde(default)]
    pub comment: Option<String>,
    /// The record form of the actor that wrote the version.
    pub created_by: String,
}

/// Update input for a Policy. Creates a new immutable version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdate {
    /// Server-resolved engine schema closure; empty for MRW.
    #[serde(default)]
    pub schema_snapshot: Option<crate::engine::PolicySchemaSnapshot>,
    /// Expected current version. Lost-update protection.
    pub if_match_version: u32,
    /// New engine identifier.
    #[serde(default)]
    pub engine_id: Option<String>,
    /// New engine configuration.
    #[serde(default)]
    pub engine_config: Option<Value>,
    /// New timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Version comment.
    #[serde(default)]
    pub comment: Option<String>,
    /// The record form of the actor that wrote the version.
    pub created_by: String,
}

/// One immutable Policy version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PolicyVersion {
    /// Server-resolved engine schema closure; empty for MRW.
    #[serde(default)]
    pub schema_snapshot: crate::engine::PolicySchemaSnapshot,
    /// Stable policy identifier.
    pub policy_id: PolicyId,
    /// Monotonic version number, first version is 1.
    pub version: u32,
    /// Scope.
    pub scope: PolicyScope,
    /// Engine identifier.
    pub engine_id: String,
    /// Engine configuration.
    pub engine_config: Value,
    /// Requested timeout in milliseconds; `None` uses the default.
    /// Clamp against the loaded operator bound on each evaluation.
    pub timeout_ms: Option<u64>,
    /// Operator description.
    pub description: Option<String>,
    /// Version state.
    pub state: PolicyVersionState,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    /// Creator identity.
    pub created_by: String,
    /// Version comment.
    pub comment: Option<String>,
}

/// Version listing entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PolicyVersionMeta {
    /// Version number.
    pub version: u32,
    /// Version state.
    pub state: PolicyVersionState,
    /// Creation time.
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    /// Creator identity.
    pub created_by: String,
    /// Version comment.
    pub comment: Option<String>,
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

/// Platform default rows of the three configuration tables (DESIGN 3.7,
/// "Bootstrap seeded state").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigDefaults {
    /// Acquisition contention timeout. `0` means fail fast.
    pub contention_timeout_ms: u64,
    /// Per-`(tenant, metric)` active-lease cap (invariant I7).
    pub max_active_leases: u32,
    /// Idempotency record retention.
    pub idempotency_retention_secs: u64,
}

impl Default for ConfigDefaults {
    fn default() -> Self {
        Self {
            contention_timeout_ms: 50,
            max_active_leases: 1000,
            idempotency_retention_secs: 86_400,
        }
    }
}

/// Everything `bootstrap()` needs. Later features extend it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapBundle {
    /// Contract major the caller was compiled against (invariant I12).
    pub contract_major: u32,
    /// Default configuration rows to seed when missing.
    #[serde(default)]
    pub config_defaults: ConfigDefaults,
    /// Seeded `global` policy. `None` until the resolution-policy-engine
    /// feature registers its engine.
    #[serde(default)]
    pub global_policy: Option<PolicyDraft>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
